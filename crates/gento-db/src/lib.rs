//! GEN-TO SQLite基盤 (MVP WP-1)。メタデータの正本を担う。
//!
//! # 構成
//!
//! - `Db`: 接続管理 (単一writer + 分離reader) とmigration実行
//! - `SqliteRepos`: `gento_core::repo` trait群のSQLite実装
//! - `Db::diagnose`: 整合性検査・診断スナップショット
//!
//! # DB並行性方針 (data-model §13・PM計画§6-5。WP-2以降もこの方針に従う)
//!
//! - **書き込みは単一writer接続に集約**する。`Db::with_writer` は内部で
//!   `Mutex`により直列化され、同時に書くスレッドは1つに制限される
//!   (scan / UI編集 / AIジョブ / backupの全書き込みがこの経路を通る)。
//! - **読み取りは書き込みと分離した接続**で行う (`Db::with_reader`)。
//!   WALモードのためreaderはwriterをブロックしない。プールは不要で、
//!   idle時2本を保持し、貸出要求が集中したときだけ追加接続を開き
//!   返却時に閉じる (定常2本・突入時のみ増える軽いプール)。
//!   reader接続は `PRAGMA query_only=ON` を設定し、SQL文レベルで
//!   書き込みを拒否する (単一writer方針の強制。repo API・生SQLの
//!   いずれでもSQLiteが `SQLITE_READONLY` を返す)。
//!   `OPEN_READ_ONLY` フラグにしないのは、WAL運用 (-shm書き込み・
//!   recovery) に書き込み権限が必要になり得るため — query_onlyは
//!   SQL文のみを拒否しpagerレベルのWAL処理は妨げない。
//! - `busy_timeout` 5000ms (既定。接続単位)。外部プロセス (ユーザーが
//!   sqlite3 CLIで開いた等) との書き込み競合時は待ってからエラー。
//! - 長時間トランザクションを避け、スキャン・メタデータ抽出・AI処理は
//!   小さなバッチでコミットする (WP-2以降の実装規約)。
//!
//! # 永続化・整合性規約
//!
//! - journal_mode = WAL、`foreign_keys = ON` (全接続で毎回設定)、
//!   `synchronous = NORMAL` (WALと組み合わせの標準推奨。アプリ異常終了で
//!   破損しない; 電源断では直近トランザクションを失い得る)。
//! - schema versionは `PRAGMA user_version`。migrationは順方向のみで
//!   1件=1トランザクション (migrate.rs)。未適用migrationがありデータ
//!   保持DBの場合、適用前に `VACUUM INTO` でWAL安全backupを作成し、
//!   integrity_checkで検証したうえで世代保持 (既定3世代) する。
//! - DBを閉じずにプロセスが落ちても、次回オープンでWALから自動回復する
//!   (クラッシュシミュレーションテストで検証)。

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use gento_core::repo::RepoError;
use rusqlite::Connection;

mod diagnose;
mod ids;
mod migrate;
mod paths;
mod repos;
mod schema;

pub use diagnose::DbStatus;
/// backup/restore正準表現 (パーセントエスケープUTF-8。WP-13で使用)。
pub use paths::{percent_decode_to_bytes, percent_encode_bytes};
pub use repos::SqliteRepos;
/// このbuildの最新schema version。
pub use schema::LATEST_VERSION;

/// 既定のbusy_timeout (ms)。PM計画§6-2。
pub(crate) const DEFAULT_BUSY_TIMEOUT_MS: u64 = 5_000;
/// readerプールのidle保持数。
const READER_IDLE_POOL: usize = 2;

/// DBファイルの既定名 (AppDirs.data_dir直下に置く)。
pub const DB_FILE_NAME: &str = "library.sqlite3";
/// backupディレクトリ名 (DBと同じ親の下)。
pub const BACKUP_DIR_NAME: &str = "db-backups";
/// backup世代保持の既定値。
pub const DEFAULT_BACKUP_GENERATIONS: usize = 3;

/// [`Db`] の開失敗。
#[derive(Debug)]
pub enum DbError {
    /// 接続・PRAGMA設定の失敗。
    Open(String),
    /// migrationの失敗 (ロールバック済み。メッセージに文脈)。
    Migration(String),
    /// 診断・クエリの失敗。
    Query(String),
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DbError::Open(msg) => write!(f, "failed to open database: {msg}"),
            DbError::Migration(msg) => write!(f, "database migration failed: {msg}"),
            DbError::Query(msg) => write!(f, "database query failed: {msg}"),
        }
    }
}

impl std::error::Error for DbError {}

/// [`Db`] オープン時のオプション。
#[derive(Debug, Clone)]
pub struct DbOptions {
    /// migration前backupの出力ディレクトリ。
    /// `None`なら DBと同じ親の `db-backups/` を使う。
    pub backup_dir: Option<PathBuf>,
    /// backup世代保持数。
    pub backup_generations: usize,
    /// busy_timeout (ms)。
    pub busy_timeout_ms: u64,
    /// readerプールのidle保持数。
    pub reader_idle_pool: usize,
}

impl Default for DbOptions {
    fn default() -> Self {
        Self {
            backup_dir: None,
            backup_generations: DEFAULT_BACKUP_GENERATIONS,
            busy_timeout_ms: DEFAULT_BUSY_TIMEOUT_MS,
            reader_idle_pool: READER_IDLE_POOL,
        }
    }
}

/// 既定のbackupディレクトリ (DBファイルと同じ親の `db-backups/`)。
pub fn default_backup_dir(db_path: &Path) -> PathBuf {
    db_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(BACKUP_DIR_NAME)
}

/// 開かれたデータベース (単一writer + readerプール)。
///
/// `Connection` は `Send` だが `Sync` でないため、全アクセスは
/// [`Db::with_writer`] / [`Db::with_reader`] 経由で直列化される。
pub struct Db {
    path: PathBuf,
    options: DbOptions,
    writer: Mutex<Connection>,
    readers: Mutex<Vec<Connection>>,
}

// ConnectionはDebugではないため手動実装 (テストのunwrap_err等で必要)。
impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db")
            .field("path", &self.path)
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

impl Db {
    /// 既定オプションで開く (新規作成・既存オープン両対応)。
    /// 親ディレクトリが無ければ作成し、migrationを最新まで適用する。
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DbError> {
        Self::open_with(path, DbOptions::default())
    }

    /// オプション指定で開く。
    pub fn open_with(path: impl AsRef<Path>, options: DbOptions) -> Result<Self, DbError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| DbError::Open(format!("create {}: {e}", parent.display())))?;
            }
        }

        let mut conn = open_connection(&path, options.busy_timeout_ms, false)?;
        let migrate_opts = migrate::MigrationOpts {
            backup_dir: Some(match &options.backup_dir {
                Some(dir) => dir.clone(),
                None => default_backup_dir(&path),
            }),
            keep_generations: options.backup_generations,
        };
        migrate::run_migrations(&mut conn, schema::MIGRATIONS, &migrate_opts)
            .map_err(|e| DbError::Migration(e.to_string()))?;

        Ok(Self {
            path,
            options,
            writer: Mutex::new(conn),
            readers: Mutex::new(Vec::new()),
        })
    }

    /// DBファイルのパス。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 現在のschema version (`PRAGMA user_version`)。
    pub fn schema_version(&self) -> Result<i32, DbError> {
        self.with_reader(|repos| {
            repos
                .conn()
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .map_err(|e| RepoError::Db(e.to_string()))
        })
        .map_err(|e| DbError::Query(e.to_string()))
    }

    /// 書き込み用repo (単一writer接続をMutexで直列化)。
    pub fn with_writer<T>(
        &self,
        f: impl FnOnce(&SqliteRepos<'_>) -> Result<T, RepoError>,
    ) -> Result<T, RepoError> {
        let conn = self
            .writer
            .lock()
            .map_err(|_| RepoError::Db("writer lock poisoned".to_string()))?;
        f(&SqliteRepos::new(&conn))
    }

    /// 読み取り用repo (writerと分離した接続。WALなのでwriterをブロックしない)。
    ///
    /// reader接続は `PRAGMA query_only=ON` のため、この中での書き込み
    /// (repo API・生SQL問わず) は `SQLITE_READONLY` で失敗する。
    /// 書き込みは [`Db::with_writer`] を使うこと。
    pub fn with_reader<T>(
        &self,
        f: impl FnOnce(&SqliteRepos<'_>) -> Result<T, RepoError>,
    ) -> Result<T, RepoError> {
        let conn = self.checkout_reader()?;
        let out = f(&SqliteRepos::new(&conn));
        self.return_reader(conn);
        out
    }

    fn checkout_reader(&self) -> Result<Connection, RepoError> {
        let mut pool = self
            .readers
            .lock()
            .map_err(|_| RepoError::Db("reader pool poisoned".to_string()))?;
        match pool.pop() {
            Some(conn) => Ok(conn),
            None => open_connection(&self.path, self.options.busy_timeout_ms, true)
                .map_err(|e| RepoError::Db(e.to_string())),
        }
    }

    fn return_reader(&self, conn: Connection) {
        if let Ok(mut pool) = self.readers.lock() {
            if pool.len() < self.options.reader_idle_pool {
                pool.push(conn);
                return;
            }
        }
        // ここへ到達するのはidle上限超過かロック毒化。いずれの場合も接続を
        // drop (= close) して資源を残さない。
        drop(conn);
    }
}

/// PRAGMA一式を設定した接続を開く (writer/reader共通の基礎部分)。
/// `journal_mode=WAL` はファイルDBでのみ有効 (in-memoryは `memory` のまま)。
/// `query_only` はreader接続向け: SQL文レベルの書き込みを拒否する
/// (flagベースのREAD_ONLYではなくPRAGMAなのは、WALのpager処理
/// (-shm書き込み・recovery) は妨げないようにするため)。
fn open_connection(
    path: &Path,
    busy_timeout_ms: u64,
    query_only: bool,
) -> Result<Connection, DbError> {
    let conn = Connection::open(path)
        .map_err(|e| DbError::Open(format!("open {}: {e}", path.display())))?;
    let _: String = conn
        .query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
        .map_err(|e| DbError::Open(format!("set journal_mode: {e}")))?;
    conn.pragma_update(None, "foreign_keys", true)
        .map_err(|e| DbError::Open(format!("set foreign_keys: {e}")))?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|e| DbError::Open(format!("set synchronous: {e}")))?;
    conn.busy_timeout(Duration::from_millis(busy_timeout_ms))
        .map_err(|e| DbError::Open(format!("set busy_timeout: {e}")))?;
    if query_only {
        // 最後に設定 (journal_mode等、設定自体に書き込みが必要なPRAGMAを
        // 先に通すため)
        conn.pragma_update(None, "query_only", true)
            .map_err(|e| DbError::Open(format!("set query_only: {e}")))?;
    }
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gento_core::repo::{AssetRepo, FileRepo, FileUpsert, LibraryRootRepo, NewAsset, ScanState};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "gento-db-lib-{tag}-{}-{}",
                std::process::id(),
                crate::ids::now_utc_compact()
            ));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn open_creates_missing_parent_directories() {
        let tmp = TempDir::new("mkdir");
        let db_path = tmp.path().join("nested/deep/db.sqlite3");
        assert!(!db_path.parent().unwrap().exists());
        let db = Db::open(&db_path).unwrap();
        assert!(db_path.exists(), "DBファイルが作成される");
        assert_eq!(db.schema_version().unwrap(), schema::LATEST_VERSION);
    }

    #[test]
    fn pragmas_are_set_on_writer_and_readers() {
        let tmp = TempDir::new("pragmas");
        let db = Db::open(tmp.path().join("db.sqlite3")).unwrap();
        db.with_reader(|repos| {
            let conn = repos.conn();
            let jm: String = conn
                .query_row("PRAGMA journal_mode", [], |r| r.get(0))
                .unwrap();
            let fk: i64 = conn
                .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
                .unwrap();
            let bt: i64 = conn
                .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
                .unwrap();
            // synchronousの戻り値は整数 (0=OFF, 1=NORMAL, 2=FULL)
            let sync: i64 = conn
                .query_row("PRAGMA synchronous", [], |r| r.get(0))
                .unwrap();
            // readerは書き込み禁止 (単一writer方針の強制)
            let qo: i64 = conn
                .query_row("PRAGMA query_only", [], |r| r.get(0))
                .unwrap();
            assert_eq!(jm, "wal");
            assert_eq!(fk, 1, "foreign_keys=ON (接続ごと)");
            assert_eq!(bt, 5_000);
            assert_eq!(sync, 1, "synchronous=NORMAL");
            assert_eq!(qo, 1, "readerはquery_only=ON");
            Ok(())
        })
        .unwrap();
        // writer側も同一PRAGMA (query_onlyのみ立たない)
        db.with_writer(|repos| {
            let fk: i64 = repos
                .conn()
                .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
                .unwrap();
            let qo: i64 = repos
                .conn()
                .query_row("PRAGMA query_only", [], |r| r.get(0))
                .unwrap();
            assert_eq!(fk, 1);
            assert_eq!(qo, 0, "writerは書き込み可能");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn reader_sees_committed_writes_and_survives_concurrent_checkouts() {
        let tmp = TempDir::new("rw");
        let db = Db::open(tmp.path().join("db.sqlite3")).unwrap();
        db.with_writer(|repos| {
            repos.add_root(Path::new("/data/p"))?;
            Ok(())
        })
        .unwrap();
        db.with_reader(|repos| {
            assert_eq!(repos.list_roots()?.len(), 1);
            Ok(())
        })
        .unwrap();

        // 同時reader貸出 (idle=2を超える突入): 3接続同時に持ち出しても動く
        let r1 = db.checkout_reader().unwrap();
        let r2 = db.checkout_reader().unwrap();
        let r3 = db.checkout_reader().unwrap();
        let count = |c: &Connection| -> usize {
            c.query_row("SELECT count(*) FROM library_roots", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap() as usize
        };
        assert_eq!(count(&r1), 1);
        assert_eq!(count(&r2), 1);
        assert_eq!(count(&r3), 1);
        db.return_reader(r1);
        db.return_reader(r2);
        db.return_reader(r3);
        // idle枠を超えた3本目は閉じられ、プールは2本に戻る
        assert_eq!(db.readers.lock().unwrap().len(), 2);
    }

    #[test]
    fn crash_simulation_wal_left_behind_recovers_on_reopen() {
        // 受入条件4: DBを閉じずに強制終了 (Connection closeなし) → 再オープンで
        // コミット済みデータが読める。std::mem::forgetでcloseを省略することで
        // プロセス死亡をシミュレートする (WAL/checkpoint未実施の状態)。
        let tmp = TempDir::new("crash");
        let db_path = tmp.path().join("db.sqlite3");
        {
            let db = Db::open(&db_path).unwrap();
            db.with_writer(|repos| {
                let root = repos.add_root(Path::new("/data/p"))?;
                let file = repos.upsert_file(FileUpsert {
                    root_id: root.id,
                    rel_path: PathBuf::from("a.png"),
                    display_name: "a.png".to_string(),
                    byte_size: 42,
                    mtime_ms: 1_700_000_000_000,
                    scan_state: ScanState::Present,
                })?;
                let asset = repos.create_asset(NewAsset {
                    content_hash: "blake3:cafe".to_string(),
                    hash_algorithm: "blake3".to_string(),
                    byte_size: 42,
                    mime_type: None,
                })?;
                repos.link_file(file.id, asset.id)?;
                Ok(())
            })
            .unwrap();
            // closeせずに「死亡」
            std::mem::forget(db);
        }

        let db2 = Db::open(&db_path).unwrap();
        assert_eq!(db2.schema_version().unwrap(), schema::LATEST_VERSION);
        db2.with_reader(|repos| {
            let roots = repos.list_roots()?;
            assert_eq!(roots.len(), 1);
            let files = repos
                .find_by_path(roots[0].id, &PathBuf::from("a.png"))?
                .expect("file survives crash");
            assert_eq!(files.byte_size, 42);
            assert!(files.asset_id.is_some(), "linkも失われていない");
            let asset = repos
                .find_by_hash("blake3:cafe")?
                .expect("asset survives crash");
            assert_eq!(asset.byte_size, 42);
            Ok(())
        })
        .unwrap();
        let status = db2.diagnose().unwrap();
        assert_eq!(status.integrity_check, "ok");
        assert_eq!(status.foreign_key_violations, 0);
    }

    #[test]
    fn reopen_after_normal_close_is_idempotent_no_backup() {
        let tmp = TempDir::new("reopen");
        let db_path = tmp.path().join("db.sqlite3");
        {
            let db = Db::open(&db_path).unwrap();
            db.with_writer(|repos| {
                repos.add_root(Path::new("/data/p"))?;
                Ok(())
            })
            .unwrap();
            drop(db); // 正常close
        }
        // 再オープン: migration冪等スキップ・backupは作られない
        let db2 = Db::open(&db_path).unwrap();
        db2.with_reader(|repos| {
            assert_eq!(repos.list_roots()?.len(), 1);
            Ok(())
        })
        .unwrap();
        let backup_dir = default_backup_dir(&db_path);
        assert!(
            !backup_dir.exists() || std::fs::read_dir(&backup_dir).unwrap().count() == 0,
            "適用済みDBの再オープンでbackupは作られない"
        );
    }

    #[test]
    fn default_backup_dir_is_sibling() {
        let p = Path::new("/x/data/library.sqlite3");
        assert_eq!(default_backup_dir(p), PathBuf::from("/x/data/db-backups"));
    }

    #[test]
    fn fts5_is_available_at_runtime() {
        // PM計画§6-2: bundled SQLiteのFTS5可用性を実行時証明する。
        // (WP-8のFTS構築の前提。feature確認だけでは不十分という指摘への対応)
        // 仮想テーブル構築は書き込みのためwith_writerで行う (readerは
        // query_onlyで書けない)。
        let tmp = TempDir::new("fts5");
        let db = Db::open(tmp.path().join("db.sqlite3")).unwrap();
        db.with_writer(|repos| {
            let conn = repos.conn();
            conn.execute_batch(
                "CREATE VIRTUAL TABLE wp1_fts_probe USING fts5(content); \
                 INSERT INTO wp1_fts_probe VALUES ('hello world'); \
                 INSERT INTO wp1_fts_probe VALUES ('gen-to sqlite');",
            )
            .map_err(|e| RepoError::Db(e.to_string()))?;
            let hit: i64 = conn
                .query_row("SELECT count(*) FROM wp1_fts_probe('sqlite')", [], |r| {
                    r.get(0)
                })
                .map_err(|e| RepoError::Db(e.to_string()))?;
            assert_eq!(hit, 1, "FTS5 MATCHクエリが動作する");
            // 補助関数 (bm25) も利用可能であること
            let rank: f64 = conn
                .query_row(
                    "SELECT bm25(wp1_fts_probe) FROM wp1_fts_probe('sqlite')",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| RepoError::Db(e.to_string()))?;
            assert!(rank.is_finite(), "bm25が数値を返す: {rank}");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn reader_connections_cannot_write() {
        let tmp = TempDir::new("ro");
        let db = Db::open(tmp.path().join("db.sqlite3")).unwrap();
        db.with_writer(|repos| {
            repos.add_root(Path::new("/data/p"))?;
            Ok(())
        })
        .unwrap();

        // repo API経由の書き込みも拒否
        let err = db
            .with_reader(|repos| repos.add_root(Path::new("/data/q")))
            .unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("readonly"),
            "reader経由の書き込みはSQLITE_READONLY: {err}"
        );

        // 生Connection経由の書き込みも同様 (engineレベルで強制)
        db.with_reader(|repos| {
            let e = repos
                .conn()
                .execute("CREATE TABLE rogue (x)", [])
                .unwrap_err();
            assert!(e.to_string().to_lowercase().contains("readonly"), "{e}");
            // 読み取りは引き続き可能
            let n: i64 = repos
                .conn()
                .query_row("SELECT count(*) FROM library_roots", [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 1);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn open_rejects_db_with_future_schema_version() {
        let tmp = TempDir::new("future");
        let db_path = tmp.path().join("db.sqlite3");
        {
            let db = Db::open(&db_path).unwrap();
            drop(db);
        }
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.pragma_update(None, "user_version", schema::LATEST_VERSION + 1)
                .unwrap();
        }
        let err = Db::open(&db_path).unwrap_err();
        assert!(matches!(err, DbError::Migration(_)), "{err}");
        assert!(
            err.to_string().contains("newer"),
            "新しいバイナリが必要な旨: {err}"
        );
        // user_versionは不変
        let conn = Connection::open(&db_path).unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v as i32, schema::LATEST_VERSION + 1);
    }

    #[test]
    fn open_error_on_unmanaged_database_is_migration_error() {
        let tmp = TempDir::new("unmanaged");
        let db_path = tmp.path().join("db.sqlite3");
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch("CREATE TABLE not_gento (x);").unwrap();
        }
        let err = Db::open(&db_path).unwrap_err();
        assert!(matches!(err, DbError::Migration(_)), "{err}");
        assert!(err.to_string().contains("unmanaged"), "{err}");
    }
}
