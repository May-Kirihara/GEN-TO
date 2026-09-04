//! migration実行器 (MVP WP-1)。
//!
//! 要件 (PM計画§6-2):
//! - 1 migration = 1トランザクション。失敗時rollback。
//! - スタートアップ冪等 (適用済みmigration = `user_version` 以下をスキップ)。
//! - `user_version` が本バイナリの最新より新しいDBは拒否する
//!   (旧バイナリで未来schemaを開き、誤解・破損させないため)。
//! - 適用前backupはWAL安全な方法 (`VACUUM INTO`)。メインDBファイルの
//!   単純コピーはWAL活性時に不整合になり得るため禁止。
//! - backupはintegrity_checkで検証し、世代保持 (既定3世代) する。
//!   ファイル名はタイムスタンプ先行の固定幅形式 — 辞書順 = 時系列に
//!   なるため、version桁数の違いで世代破棄順が崩れない。
//! - `user_version = 0` で非管理オブジェクト (テーブル・view・trigger) が
//!   存在するDBは「非管理DB」として拒否する (他アプリのDB・破損DBを
//!   上書きしない)。

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::ids::now_utc_compact;
use crate::schema::Migration;

/// migration実行のオプション。
#[derive(Debug, Clone)]
pub(crate) struct MigrationOpts {
    /// backup出力ディレクトリ。`None` ならbackupを作らない
    /// (データがまだ無い新規DB相当。テストでも使用)。
    pub backup_dir: Option<PathBuf>,
    /// backup世代保持数。
    pub keep_generations: usize,
}

impl Default for MigrationOpts {
    fn default() -> Self {
        Self {
            backup_dir: None,
            keep_generations: 3,
        }
    }
}

/// migration実行結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MigrationOutcome {
    /// 適用したmigration version (昇順)。
    pub applied: Vec<i32>,
    /// 作成したbackupのパス。
    pub backup: Option<PathBuf>,
}

/// migration実行の失敗。
#[derive(Debug)]
pub(crate) enum MigrationError {
    /// 下層のSQL/IOエラー (文脈はDisplayに含む)。
    Internal(String),
    /// DBの `user_version` が本バイナリの最新より新しい。
    /// 旧バイナリでの未来schemaアクセスを防ぐため開けない。
    NewerSchema { found: i32, latest: i32 },
    /// user_version=0 なのにSQLite内部オブジェクト以外が存在する
    /// (テーブル・view・trigger)。
    UnmanagedDatabase { objects: Vec<String> },
    /// migration一覧自体の不整合 (versionが1開始連続でない)。
    BadMigrationList(String),
}

impl std::fmt::Display for MigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MigrationError::Internal(msg) => write!(f, "{msg}"),
            MigrationError::NewerSchema { found, latest } => write!(
                f,
                "database schema version {found} is newer than this binary supports \
                 (latest {latest}); a newer GEN-TO binary is required"
            ),
            MigrationError::UnmanagedDatabase { objects } => write!(
                f,
                "database contains unmanaged schema objects {:?} with schema version 0; \
                 refusing to migrate (not created by GEN-TO?)",
                objects
            ),
            MigrationError::BadMigrationList(msg) => write!(f, "bad migration list: {msg}"),
        }
    }
}

impl std::error::Error for MigrationError {}

impl From<rusqlite::Error> for MigrationError {
    fn from(e: rusqlite::Error) -> Self {
        MigrationError::Internal(format!("sqlite error: {e}"))
    }
}

impl From<std::io::Error> for MigrationError {
    fn from(e: std::io::Error) -> Self {
        MigrationError::Internal(format!("io error: {e}"))
    }
}

/// backupファイル名の接頭辞 (世代保持の回収対象マーカー)。
const BACKUP_PREFIX: &str = "library-pre-";
const BACKUP_SUFFIX: &str = ".sqlite3";

/// backupファイル名: `library-pre-{時刻}-v{version}.sqlite3`。
/// 時刻は固定幅19文字 (`YYYYMMDDTHHMMSSmmmZ`) で**先頭**に置く —
/// 辞書順 = 時系列が成立し、`v{version}` の桁数混在 (v9 と v10 等) で
/// ソート順が崩れない。`v{version}` を先頭にした旧形式はこの性質が
/// 壊れるため採用しない。
fn backup_file_name(timestamp: &str, from_version: i32) -> String {
    format!("{BACKUP_PREFIX}{timestamp}-v{from_version}{BACKUP_SUFFIX}")
}

/// migrationを実行する。
///
/// - DBの `user_version` がmigration一覧の最新より新ければ拒否する
///   (旧バイナリで未来schemaを開かせない)。
/// - 適用済み (`version <= user_version`) はスキップ (冪等)。
/// - 未適用があり、かつ `user_version > 0` (データ保有の可能性) で
///   `backup_dir` が指定されている場合、まずbackupを作る。
/// - 各migrationを1トランザクションで適用し `user_version` を進める。
pub(crate) fn run_migrations(
    conn: &mut Connection,
    migrations: &[Migration],
    opts: &MigrationOpts,
) -> Result<MigrationOutcome, MigrationError> {
    validate_migration_list(migrations)?;

    let current = read_user_version(conn)?;
    let latest = migrations.last().map(|m| m.version).unwrap_or(0);
    if current > latest {
        return Err(MigrationError::NewerSchema {
            found: current,
            latest,
        });
    }
    let pending: Vec<&Migration> = migrations.iter().filter(|m| m.version > current).collect();
    if pending.is_empty() {
        return Ok(MigrationOutcome {
            applied: Vec::new(),
            backup: None,
        });
    }

    if current == 0 {
        // この時点で未適用migrationがある = 初回。他オブジェクトが残って
        // いれば本アプリが作ったDBではないため拒否する。
        let objects = non_internal_schema_objects(conn)?;
        if !objects.is_empty() {
            return Err(MigrationError::UnmanagedDatabase { objects });
        }
    }

    let backup = if current > 0 {
        match &opts.backup_dir {
            Some(dir) => Some(create_backup(conn, dir, opts.keep_generations, current)?),
            None => None,
        }
    } else {
        None
    };

    for m in pending {
        apply_one(conn, m)?;
    }

    Ok(MigrationOutcome {
        applied: migrations
            .iter()
            .filter(|m| m.version > current)
            .map(|m| m.version)
            .collect(),
        backup,
    })
}

fn validate_migration_list(migrations: &[Migration]) -> Result<(), MigrationError> {
    for (index, m) in migrations.iter().enumerate() {
        let expected = index as i32 + 1;
        if m.version != expected {
            return Err(MigrationError::BadMigrationList(format!(
                "migration versions must start at 1 and be contiguous; \
                 expected {expected}, got {}",
                m.version
            )));
        }
    }
    Ok(())
}

fn read_user_version(conn: &Connection) -> Result<i32, MigrationError> {
    let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    i32::try_from(v)
        .map_err(|_| MigrationError::Internal(format!("user_version out of range: {v}")))
}

/// 非管理DB検出: `sqlite_master` 上のテーブル・view・triggerのうち
/// SQLite内部 (`sqlite_%`) 以外の名前。indexは他オブジェクトに付随して
/// 作られ得るため対象外 (検出の意味を持つオブジェクト種に限定)。
fn non_internal_schema_objects(conn: &Connection) -> Result<Vec<String>, MigrationError> {
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master \
         WHERE type IN ('table', 'view', 'trigger') \
         AND name NOT LIKE 'sqlite_%' \
         ORDER BY name",
    )?;
    let objects = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(objects)
}

fn apply_one(conn: &mut Connection, m: &Migration) -> Result<(), MigrationError> {
    let tx = conn.transaction()?;
    let result = tx
        .execute_batch(m.sql)
        .and_then(|_| tx.pragma_update(None, "user_version", m.version));
    if let Err(e) = result {
        // tx dropでもrollbackされるが、明示して失敗理由を排しておく
        let _ = tx.rollback();
        return Err(MigrationError::Internal(format!(
            "migration v{} ({}) failed and was rolled back: {e}",
            m.version, m.name
        )));
    }
    tx.commit()?;
    Ok(())
}

/// WAL安全backup: `VACUUM INTO` で独立した新規ファイルへ書き出し、
/// integrity_checkで検証したうえで世代保持する。
fn create_backup(
    conn: &Connection,
    dir: &Path,
    keep: usize,
    from_version: i32,
) -> Result<PathBuf, MigrationError> {
    std::fs::create_dir_all(dir)?;
    let dest = dir.join(backup_file_name(&now_utc_compact(), from_version));
    let dest_str = dest.to_string_lossy().into_owned();
    conn.execute("VACUUM INTO ?1", [dest_str.as_str()])?;

    // 検証: 開いてintegrity_check。失敗なら移行を止める。
    // READ_ONLYフラグを付けない: FTS5テーブル (asset_docs, WP-8) の
    // integrity検査は内部で書き込みを試みるため、読み取り専用接続では
    // 「attempt to write a readonly database」で検査が失敗する。
    let check = Connection::open(&dest)?;
    let result: String = check.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    drop(check);
    if result != "ok" {
        // 不正backupは次回の世代計算を汚さないよう削除する
        let _ = std::fs::remove_file(&dest);
        return Err(MigrationError::Internal(format!(
            "backup integrity check failed: {result}"
        )));
    }

    rotate_backups(dir, keep)?;
    Ok(dest)
}

/// `library-pre-*.sqlite3` のうち新しい方から `keep` 世代を残し、
/// 残りを削除する。ファイル名はタイムスタンプ先行の固定幅形式のため、
/// 名前のソート = 時系列 (version桁数の違いは順序に影響しない)。
fn rotate_backups(dir: &Path, keep: usize) -> Result<(), MigrationError> {
    let mut backups: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(BACKUP_PREFIX) && n.ends_with(BACKUP_SUFFIX))
                .unwrap_or(false)
        })
        .collect();
    backups.sort(); // プレフィックス固定 + タイムスタンプ先行のため時系列順
    while backups.len() > keep {
        let oldest = backups.remove(0);
        std::fs::remove_file(&oldest)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{LATEST_VERSION, MIGRATIONS};
    use std::time::Duration;

    /// テスト用テンポラリディレクトリ (既存コードの規約に従い
    /// std::env::temp_dir + プロセスID + 名前。Dropで掃除)。
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "gento-db-migrate-{tag}-{}-{}",
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

    fn failing_migration_v10() -> Migration {
        Migration {
            version: 10,
            name: "test_failing",
            sql: "CREATE TABLE wp10_test_extra (id INTEGER PRIMARY KEY); INSERT INTO wp10_test_extra VALUES (1); INSERT INTO nonexistent_table VALUES (1);",
        }
    }

    fn ok_migration_v10() -> Migration {
        Migration {
            version: 10,
            name: "test_ok_extra",
            sql: "CREATE TABLE wp10_test_extra (id INTEGER PRIMARY KEY);",
        }
    }

    #[test]
    fn fresh_db_creates_latest_schema_and_sets_user_version() {
        let tmp = TempDir::new("fresh");
        let mut conn = Connection::open(tmp.path().join("db.sqlite3")).unwrap();
        let outcome = run_migrations(&mut conn, MIGRATIONS, &MigrationOpts::default()).unwrap();
        assert_eq!(outcome.applied, vec![1, 2, 3, 4, 5, 6, 7, 8, 9]);
        assert_eq!(outcome.backup, None); // 新規DB (v0) はbackup不要
        assert_eq!(read_user_version(&conn).unwrap(), LATEST_VERSION);

        // 14テーブル + 主要インデックスが存在する
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap();
        let tables: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for t in crate::schema::TABLES {
            assert!(tables.contains(&t.to_string()), "missing table {t}");
        }
        let mut idx = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='index' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .unwrap();
        let indexes: Vec<String> = idx
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for expect in [
            "idx_files_asset_id",
            "idx_files_scan_state",
            "idx_asset_tags_tag_id",
            "idx_captions_current_unique",
            "idx_jobs_status_priority",
            // migration #2 (WP-2)
            "idx_files_root_scan_state",
            "idx_files_os_file_id",
            // migration #3 (FG-WP2)
            "idx_files_root_os_file_id",
        ] {
            assert!(
                indexes.contains(&expect.to_string()),
                "missing index {expect}"
            );
        }

        // migration #4 (FG-WP2-2): files.mtime_ns 列 (NULL可)
        let mut stmt = conn.prepare("PRAGMA table_info(files)").unwrap();
        let files_columns: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            files_columns.contains(&"mtime_ns".to_string()),
            "missing files.mtime_ns: {files_columns:?}"
        );
    }

    #[test]
    fn already_migrated_db_is_noop() {
        let tmp = TempDir::new("idempotent");
        let mut conn = Connection::open(tmp.path().join("db.sqlite3")).unwrap();
        run_migrations(&mut conn, MIGRATIONS, &MigrationOpts::default()).unwrap();
        // データ投入
        conn.execute(
            "INSERT INTO library_roots (uuid, path, path_encoding, display_path, created_at, updated_at)
             VALUES ('00000000-0000-7000-8000-000000000001', x'2f61', 'utf8', '/a', '2026-08-30T00:00:00.000Z', '2026-08-30T00:00:00.000Z')",
            [],
        )
        .unwrap();

        // 再実行 → 冪等スキップ・backupなし・データ不変
        let outcome = run_migrations(&mut conn, MIGRATIONS, &MigrationOpts::default()).unwrap();
        assert!(outcome.applied.is_empty());
        assert_eq!(outcome.backup, None);
        let n: i64 = conn
            .query_row("SELECT count(*) FROM library_roots", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn v2_database_upgrades_to_v4_with_backup() {
        // WP-2世代 (v2) のデータ保有DBは、最新 (v4) 適用前にbackupを作って
        // から移行される (migration #3+#4)。
        let tmp = TempDir::new("v2to4");
        let backup_dir = tmp.path().join("db-backups");
        let mut conn = Connection::open(tmp.path().join("db.sqlite3")).unwrap();
        run_migrations(&mut conn, &MIGRATIONS[..2], &MigrationOpts::default()).unwrap();
        conn.execute(
            "INSERT INTO library_roots (uuid, path, path_encoding, display_path, created_at, updated_at)
             VALUES ('00000000-0000-7000-8000-000000000004', x'2f64', 'utf8', '/d',
                     '2026-08-30T00:00:00.000Z', '2026-08-30T00:00:00.000Z')",
            [],
        )
        .unwrap();

        let opts = MigrationOpts {
            backup_dir: Some(backup_dir.clone()),
            keep_generations: 3,
        };
        let outcome = run_migrations(&mut conn, MIGRATIONS, &opts).unwrap();
        assert_eq!(outcome.applied, vec![3, 4, 5, 6, 7, 8, 9]);
        assert_eq!(read_user_version(&conn).unwrap(), LATEST_VERSION);
        let backup = outcome.backup.expect("v2からの移行でbackup作成");
        assert!(backup.starts_with(&backup_dir));

        // v3の複合indexとv4のmtime_ns列が存在
        let idx: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='index' AND name='idx_files_root_os_file_id'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(idx, 1);
        let has_ns: i64 = conn
            .query_row(
                "SELECT count(*) FROM pragma_table_info('files') WHERE name='mtime_ns'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_ns, 1);
        // データは無傷
        let n: i64 = conn
            .query_row("SELECT count(*) FROM library_roots", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn v3_database_upgrades_to_v4_with_backup() {
        // FG-WP2世代 (v3) のDBは、#4 (files.mtime_ns列追加) 適用前にbackupを
        // 作ってから移行される。既存行のmtime_nsはNULL (次の走査で充填)。
        let tmp = TempDir::new("v3to4");
        let backup_dir = tmp.path().join("db-backups");
        let mut conn = Connection::open(tmp.path().join("db.sqlite3")).unwrap();
        run_migrations(&mut conn, &MIGRATIONS[..3], &MigrationOpts::default()).unwrap();
        conn.execute(
            "INSERT INTO library_roots (uuid, path, path_encoding, display_path, created_at, updated_at)
             VALUES ('00000000-0000-7000-8000-000000000005', x'2f65', 'utf8', '/e',
                     '2026-08-30T00:00:00.000Z', '2026-08-30T00:00:00.000Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO files (uuid, root_id, rel_path, path_encoding, display_name,
                                byte_size, mtime_ms, scan_state, last_seen_at, created_at, updated_at)
             VALUES ('00000000-0000-7000-8000-000000000006', 1, x'612e706e67', 'utf8', 'a.png',
                     10, 5, 'present', '2026-08-30T00:00:00.000Z',
                     '2026-08-30T00:00:00.000Z', '2026-08-30T00:00:00.000Z')",
            [],
        )
        .unwrap();

        let opts = MigrationOpts {
            backup_dir: Some(backup_dir.clone()),
            keep_generations: 3,
        };
        let outcome = run_migrations(&mut conn, MIGRATIONS, &opts).unwrap();
        assert_eq!(outcome.applied, vec![4, 5, 6, 7, 8, 9]);
        assert_eq!(read_user_version(&conn).unwrap(), LATEST_VERSION);
        assert!(outcome
            .backup
            .expect("backup created")
            .starts_with(&backup_dir));

        // 既存行は無傷でmtime_nsはNULL (後方互換。次の走査で充填)
        let (count, ns): (i64, Option<i64>) = conn
            .query_row("SELECT count(*), max(mtime_ns) FROM files", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(ns, None);
    }

    #[test]
    fn unmanaged_db_with_tables_is_rejected() {
        let tmp = TempDir::new("unmanaged");
        let mut conn = Connection::open(tmp.path().join("db.sqlite3")).unwrap();
        conn.execute_batch("CREATE TABLE someone_elses (x);")
            .unwrap();
        let err = run_migrations(&mut conn, MIGRATIONS, &MigrationOpts::default()).unwrap_err();
        match &err {
            MigrationError::UnmanagedDatabase { objects } => {
                assert_eq!(objects, &vec!["someone_elses".to_string()]);
            }
            other => panic!("unexpected error: {other:?}"),
        }
        // 破壊されていない
        let n: i64 = conn
            .query_row("SELECT count(*) FROM someone_elses", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn failing_migration_rolls_back_and_keeps_backup() {
        let tmp = TempDir::new("interrupted");
        let backup_dir = tmp.path().join("db-backups");
        let db_path = tmp.path().join("db.sqlite3");

        // v1適用済みDBを用意 (データ付き)
        let mut conn = Connection::open(&db_path).unwrap();
        run_migrations(&mut conn, MIGRATIONS, &MigrationOpts::default()).unwrap();
        conn.execute(
            "INSERT INTO library_roots (uuid, path, path_encoding, display_path, created_at, updated_at)
             VALUES ('00000000-0000-7000-8000-000000000002', x'2f62', 'utf8', '/b', '2026-08-30T00:00:00.000Z', '2026-08-30T00:00:00.000Z')",
            [],
        )
        .unwrap();

        // 実migration一覧の後に、途中で失敗するテストmigrationを注入して実行
        let mut list: Vec<Migration> = MIGRATIONS.to_vec();
        list.push(failing_migration_v10());
        let opts = MigrationOpts {
            backup_dir: Some(backup_dir.clone()),
            keep_generations: 3,
        };
        let err = run_migrations(&mut conn, &list, &opts).unwrap_err();
        assert!(err.to_string().contains("rolled back"), "error: {err}");

        // user_version不変・データ無傷・失敗migrationのテーブルは残っていない
        assert_eq!(read_user_version(&conn).unwrap(), LATEST_VERSION);
        let n: i64 = conn
            .query_row("SELECT count(*) FROM library_roots", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        let exists: bool = conn
            .query_row(
                "SELECT count(*) > 0 FROM sqlite_master WHERE name='wp10_test_extra'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!exists, "rollbackされたテーブルが残っている");

        // backupが作成され、単独で開ける完全なDBであること
        let backups = list_backups(&backup_dir);
        assert_eq!(backups.len(), 1, "backup exactly one");
        let bk = Connection::open(&backups[0]).unwrap();
        let ic: String = bk
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ic, "ok");
        let roots: i64 = bk
            .query_row("SELECT count(*) FROM library_roots", [], |r| r.get(0))
            .unwrap();
        assert_eq!(roots, 1, "backupは移行前データを保持");

        // 再実行 (今度は成功するテストmigration) で最新→+1へ進めることもできる
        let mut list_ok: Vec<Migration> = MIGRATIONS.to_vec();
        list_ok.push(ok_migration_v10());
        let outcome = run_migrations(&mut conn, &list_ok, &opts).unwrap();
        assert_eq!(outcome.applied, vec![LATEST_VERSION + 1]);
        assert_eq!(read_user_version(&conn).unwrap(), LATEST_VERSION + 1);
    }

    #[test]
    fn successful_followup_migration_also_creates_backup() {
        let tmp = TempDir::new("followup");
        let backup_dir = tmp.path().join("db-backups");
        let mut conn = Connection::open(tmp.path().join("db.sqlite3")).unwrap();
        run_migrations(&mut conn, MIGRATIONS, &MigrationOpts::default()).unwrap();

        let mut list: Vec<Migration> = MIGRATIONS.to_vec();
        list.push(ok_migration_v10());
        let opts = MigrationOpts {
            backup_dir: Some(backup_dir.clone()),
            keep_generations: 3,
        };
        let outcome = run_migrations(&mut conn, &list, &opts).unwrap();
        assert_eq!(outcome.applied, vec![LATEST_VERSION + 1]);
        let backup = outcome.backup.expect("backup created");
        assert!(backup.starts_with(&backup_dir));
        assert_eq!(list_backups(&backup_dir).len(), 1);
    }

    #[test]
    fn backup_generations_are_rotated_to_limit() {
        let tmp = TempDir::new("rotate");
        let backup_dir = tmp.path().join("db-backups");
        std::fs::create_dir_all(&backup_dir).unwrap();
        // 既存3世代 (keep=3) + 今回1件 → 最古1件が削除され3世代になる
        for ts in [
            "20260101T000000000Z",
            "20260201T000000000Z",
            "20260301T000000000Z",
        ] {
            std::fs::write(backup_dir.join(backup_file_name(ts, 1)), b"dummy").unwrap();
        }

        let mut conn = Connection::open(tmp.path().join("db.sqlite3")).unwrap();
        run_migrations(&mut conn, MIGRATIONS, &MigrationOpts::default()).unwrap();
        let mut list: Vec<Migration> = MIGRATIONS.to_vec();
        list.push(ok_migration_v10());
        let opts = MigrationOpts {
            backup_dir: Some(backup_dir.clone()),
            keep_generations: 3,
        };
        run_migrations(&mut conn, &list, &opts).unwrap();

        let mut remaining = list_backups(&backup_dir);
        remaining.sort();
        assert_eq!(remaining.len(), 3, "keep=3");
        let names: Vec<String> = remaining
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(
            !names.iter().any(|n| n.contains("20260101")),
            "最古世代が削除されている: {names:?}"
        );
        assert!(
            names.iter().any(|n| n.contains("20260201")),
            "新しい世代は保持: {names:?}"
        );
        // バックアップでないファイルは触らない
        let keeper = backup_dir.join("important.txt");
        std::fs::write(&keeper, b"x").unwrap();
        rotate_backups(&backup_dir, 3).unwrap();
        assert!(keeper.exists());
    }

    #[test]
    fn unmanaged_db_with_view_is_rejected() {
        let tmp = TempDir::new("unmanaged-view");
        let mut conn = Connection::open(tmp.path().join("db.sqlite3")).unwrap();
        conn.execute_batch("CREATE VIEW someone_elses_view AS SELECT 1 AS x;")
            .unwrap();
        let err = run_migrations(&mut conn, MIGRATIONS, &MigrationOpts::default()).unwrap_err();
        assert!(
            matches!(err, MigrationError::UnmanagedDatabase { .. }),
            "{err}"
        );
        // 破壊されていない (viewはそのまま残る)
        let n: i64 = conn
            .query_row("SELECT count(*) FROM someone_elses_view", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn unmanaged_db_with_trigger_is_rejected() {
        // triggerも検出対象であることの直接検証 (tableだけなら旧実装でも
        // 検出済み。objectsにtrigger名が乗ることを確認する)。
        let tmp = TempDir::new("unmanaged-trigger");
        let mut conn = Connection::open(tmp.path().join("db.sqlite3")).unwrap();
        conn.execute_batch(
            "CREATE TABLE someone_elses (x);\
             CREATE TRIGGER someone_elses_trg AFTER INSERT ON someone_elses \
             BEGIN SELECT 1; END;",
        )
        .unwrap();
        let err = run_migrations(&mut conn, MIGRATIONS, &MigrationOpts::default()).unwrap_err();
        match &err {
            MigrationError::UnmanagedDatabase { objects } => {
                assert!(
                    objects.contains(&"someone_elses".to_string()),
                    "{objects:?}"
                );
                assert!(
                    objects.contains(&"someone_elses_trg".to_string()),
                    "triggerも検出対象: {objects:?}"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
        // 破壊されていない (triggerはそのまま残る)
        let remains: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'someone_elses_trg'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(remains, 1);
    }

    #[test]
    fn library_roots_settings_columns_exist_with_defaults() {
        // data-model §2.3のルート設定列。WP-2のwatcherが直ちに使用する。
        let tmp = TempDir::new("rootsettings");
        let mut conn = Connection::open(tmp.path().join("db.sqlite3")).unwrap();
        run_migrations(&mut conn, MIGRATIONS, &MigrationOpts::default()).unwrap();
        conn.execute(
            "INSERT INTO library_roots (uuid, path, path_encoding, display_path, created_at, updated_at)
             VALUES ('00000000-0000-7000-8000-000000000003', x'2f63', 'utf8', '/c',
                     '2026-08-30T00:00:00.000Z', '2026-08-30T00:00:00.000Z')",
            [],
        )
        .unwrap();
        let (watch, ai, exif, gps, settings): (i64, i64, i64, i64, Option<String>) = conn
            .query_row(
                "SELECT watch_enabled, ai_enabled, send_exif, send_gps, settings_json
                 FROM library_roots",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!((watch, ai, exif, gps), (1, 0, 0, 0), "既定値");
        assert_eq!(settings, None, "settings_jsonはNULL可");

        // 0/1以外はCHECKで拒否
        let err = conn
            .execute("UPDATE library_roots SET watch_enabled = 2", [])
            .unwrap_err();
        assert!(err.to_string().contains("CHECK"), "{err}");
    }

    #[test]
    fn migration_v8_adds_ai_run_id_columns_and_defers_no_longer() {
        // #1ではai_run_id列を持たせず (既存列へのFK付与はテーブル再構築が
        // 必要なため)、WP-10の#8でai_runs作成と同時にADD COLUMNで追加する
        // (NULL既定ならFK参照を付与可能)。#7適用時点では列が無く、
        // 全migration適用後はFK付きで存在することを検証する。
        let tmp = TempDir::new("airunid");
        let mut conn = Connection::open(tmp.path().join("db.sqlite3")).unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        run_migrations(&mut conn, &MIGRATIONS[..7], &MigrationOpts::default()).unwrap();
        for table in ["asset_tags", "captions"] {
            let mut stmt = conn
                .prepare(&format!("PRAGMA table_info({table})"))
                .unwrap();
            let columns: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            assert!(
                !columns.contains(&"ai_run_id".to_string()),
                "{table} に ai_run_id 列が存在する: {columns:?}"
            );
        }

        // #8適用: 列がFK付きで追加される
        run_migrations(&mut conn, MIGRATIONS, &MigrationOpts::default()).unwrap();
        for table in ["asset_tags", "captions"] {
            let fk_list: Vec<(i64, String)> = {
                let mut stmt = conn
                    .prepare(&format!("PRAGMA foreign_key_list({table})"))
                    .unwrap();
                stmt.query_map([], |r| Ok((r.get(0)?, r.get(2)?)))
                    .unwrap()
                    .collect::<Result<_, _>>()
                    .unwrap()
            };
            assert!(
                fk_list
                    .iter()
                    .any(|(id, to)| to == "ai_runs" && *id != i64::MAX),
                "{table} のai_run_id FKがai_runsを指す: {fk_list:?}"
            );
        }
        // ai_runs自身のFK (asset CASCADE / profile SET NULL) も検証
        // foreign_key_list: (id, seq, table, from, to, on_update, on_delete, ..)
        let mut stmt = conn.prepare("PRAGMA foreign_key_list(ai_runs)").unwrap();
        let runs_fk: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get(2)?, r.get(6)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            runs_fk.contains(&("assets".to_string(), "CASCADE".to_string())),
            "{runs_fk:?}"
        );
        assert!(
            runs_fk.contains(&("ai_profiles".to_string(), "SET NULL".to_string())),
            "{runs_fk:?}"
        );
        // jobs表のretry_waitingがCHECKで許可される
        conn.execute(
            "INSERT INTO jobs (uuid, job_type, target_type, target_id, status, \
             created_at, updated_at) VALUES \
             ('00000000-0000-7000-8000-0000000000aa', 'ai_caption_tags', 'asset', 1, \
              'retry_waiting', '2026-09-02T00:00:00.000Z', '2026-09-02T00:00:00.000Z')",
            [],
        )
        .unwrap();
    }

    #[test]
    fn v7_populated_jobs_migrate_to_v8_with_legacy_and_duplicate_rows() {
        // レガシー (v7以前) のjobs表にはjob_type CHECKが無く、pending重複も
        // 成立し得る。#8のjob_type CHECK・部分UNIQUE indexがこれらの行で
        // 移行をrollbackさせないこと (Codexレビュー: populated v7移行)。
        let tmp = TempDir::new("v7jobs");
        let mut conn = Connection::open(tmp.path().join("db.sqlite3")).unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        run_migrations(&mut conn, &MIGRATIONS[..7], &MigrationOpts::default()).unwrap();

        let insert_job = |id: i64, uuid_suffix: i64, job_type: &str, target: i64, status: &str| {
            conn.execute(
                "INSERT INTO jobs (id, uuid, job_type, target_type, target_id, status, \
                     priority, attempts, max_attempts, payload, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 'asset', ?4, ?5, 5, 0, 3, '{}',
                         '2026-09-01T00:00:00.000Z', '2026-09-01T00:00:00.000Z')",
                rusqlite::params![
                    id,
                    format!("00000000-0000-7000-8000-{uuid_suffix:012}"),
                    job_type,
                    target,
                    status
                ],
            )
            .unwrap();
        };
        // (a) pending重複 (同一job_type+target_id): id 1,2 — 最古のみ残る
        insert_job(1, 1, "ai_caption_tags", 10, "queued");
        insert_job(2, 2, "ai_caption_tags", 10, "queued");
        // (b) レガシーjob_type (実行器なし): 除去される
        insert_job(3, 3, "hash_scan", 11, "queued");
        // (c) 正常行: pending・terminal混在 (同一keyのsucceeded+queuedは共存可)
        insert_job(4, 4, "ai_reparse", 12, "queued");
        insert_job(5, 5, "ai_reparse", 12, "succeeded");
        // (d) pending→terminalの順で並ぶ同一key (id 6がpendingの最古)。
        // v7のCHECKにretry_waitingはまだ無いため重複はqueuedで作る
        insert_job(6, 6, "ai_caption_tags", 13, "running");
        insert_job(7, 7, "ai_caption_tags", 13, "queued");
        // (e) レガシー型のpending重複 — 両方とも除去される
        insert_job(8, 8, "legacy_embed", 14, "queued");
        insert_job(9, 9, "legacy_embed", 14, "queued");

        run_migrations(&mut conn, MIGRATIONS, &MigrationOpts::default()).unwrap();
        assert_eq!(read_user_version(&conn).unwrap(), LATEST_VERSION);

        let rows: Vec<(i64, String, String)> = {
            let mut stmt = conn
                .prepare("SELECT id, job_type, status FROM jobs ORDER BY id")
                .unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        // 残る行: id 1 (重複の最古), 4, 5, 6 (runningのままコピー — 起動回復で
        // queuedへ戻る)。id 2 (重複2行目), 3/8/9 (未対応型), 7 (id 6とpending重複)
        // は除外。
        let ids: Vec<i64> = rows.iter().map(|(id, _, _)| *id).collect();
        assert_eq!(ids, vec![1, 4, 5, 6], "rows: {rows:?}");
        // 部分UNIQUE indexが作成済みであること (重複なしで移行が完結した証)
        let idx: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master \
                 WHERE type='index' AND name='idx_jobs_pending_dedupe'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(idx, 1);
        // 移行後のキュー操作が正常に機能すること
        conn.execute(
            "INSERT INTO jobs (uuid, job_type, target_type, target_id, status, \
             created_at, updated_at) VALUES \
             ('00000000-0000-7000-8000-0000000000bb', 'ai_caption_tags', 'asset', 99, \
              'queued', '2026-09-02T00:00:00.000Z', '2026-09-02T00:00:00.000Z')",
            [],
        )
        .unwrap();
        // レガシー型は新規投入時にCHECKへ拒否される
        let err = conn
            .execute(
                "INSERT INTO jobs (uuid, job_type, target_type, target_id, status, \
                 created_at, updated_at) VALUES \
                 ('00000000-0000-7000-8000-0000000000cc', 'hash_scan', 'asset', 99, \
                  'queued', '2026-09-02T00:00:00.000Z', '2026-09-02T00:00:00.000Z')",
                [],
            )
            .unwrap_err();
        assert!(err.to_string().contains("CHECK"), "{err}");
    }

    #[test]
    fn newer_schema_db_is_rejected() {
        let tmp = TempDir::new("future");
        let mut conn = Connection::open(tmp.path().join("db.sqlite3")).unwrap();
        run_migrations(&mut conn, MIGRATIONS, &MigrationOpts::default()).unwrap();
        conn.pragma_update(None, "user_version", LATEST_VERSION + 1)
            .unwrap();

        let err = run_migrations(&mut conn, MIGRATIONS, &MigrationOpts::default()).unwrap_err();
        match &err {
            MigrationError::NewerSchema { found, latest } => {
                assert_eq!(*found, LATEST_VERSION + 1);
                assert_eq!(*latest, LATEST_VERSION);
            }
            other => panic!("unexpected error: {other:?}"),
        }
        assert!(err.to_string().contains("newer"), "{err}");
        // user_versionもDB内容も不変
        assert_eq!(read_user_version(&conn).unwrap(), LATEST_VERSION + 1);
    }

    #[test]
    fn backup_names_are_timestamp_first_fixed_width() {
        // タイムスタンプが先頭・固定幅であることが世代管理の前提
        assert_eq!(
            backup_file_name("20260830T010203004Z", 9),
            "library-pre-20260830T010203004Z-v9.sqlite3"
        );
    }

    #[test]
    fn backup_rotation_with_mixed_digit_versions_is_chronological() {
        // version桁数が混在 (1, 2, 9, 10) しても、時系列 (=タイムスタンプ順)
        // で古い側から破棄されること。時刻順は t1 < t2 < t3 < t4で、
        // 数値version順とは無関係に並べる。
        let tmp = TempDir::new("mixedigits");
        let backup_dir = tmp.path().join("db-backups");
        std::fs::create_dir_all(&backup_dir).unwrap();
        let entries = [
            ("20260101T000000000Z", 9),
            ("20260201T000000000Z", 10),
            ("20260301T000000000Z", 1),
            ("20260401T000000000Z", 2),
        ];
        for (ts, v) in entries {
            std::fs::write(backup_dir.join(backup_file_name(ts, v)), b"dummy").unwrap();
        }

        rotate_backups(&backup_dir, 2).unwrap();

        let mut remaining = list_backups(&backup_dir);
        remaining.sort();
        let names: Vec<String> = remaining
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 2, "keep=2: {names:?}");
        // 残るのは時系列で新しい t3(v1) と t4(v2)。桁数の大きい
        // v10/v9が時系列古い側として破棄される
        assert!(
            names
                .iter()
                .any(|n| n.contains("20260301") && n.contains("-v1")),
            "{names:?}"
        );
        assert!(
            names
                .iter()
                .any(|n| n.contains("20260401") && n.contains("-v2")),
            "{names:?}"
        );
        assert!(
            !names
                .iter()
                .any(|n| n.contains("20260101") || n.contains("20260201")),
            "時系列で古い世代 (v9/v10) が破棄: {names:?}"
        );
    }

    #[test]
    fn bad_migration_list_is_rejected() {
        let tmp = TempDir::new("badlist");
        let mut conn = Connection::open(tmp.path().join("db.sqlite3")).unwrap();
        let bad = [Migration {
            version: 2,
            name: "gap",
            sql: "CREATE TABLE x(id);",
        }];
        let err = run_migrations(&mut conn, &bad, &MigrationOpts::default()).unwrap_err();
        assert!(matches!(err, MigrationError::BadMigrationList(_)));
    }

    #[test]
    fn vacuum_backup_is_wal_safe_and_standalone() {
        // WALモードで未checkpointの書き込みがあっても、VACUUM INTOの
        // backupにはその内容が含まれる (メインファイル単体コピーとの差)。
        let tmp = TempDir::new("wal");
        let db_path = tmp.path().join("db.sqlite3");
        let mut conn = Connection::open(&db_path).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        run_migrations(&mut conn, MIGRATIONS, &MigrationOpts::default()).unwrap();
        for i in 0..50 {
            conn.execute(
                "INSERT INTO library_roots (uuid, path, path_encoding, display_path, created_at, updated_at)
                 VALUES (?1, ?2, 'utf8', ?3, '2026-08-30T00:00:00.000Z', '2026-08-30T00:00:00.000Z')",
                rusqlite::params![
                    format!("00000000-0000-7000-8000-{i:012}"),
                    format!("/r{i}").into_bytes(),
                    format!("/r{i}"),
                ],
            )
            .unwrap();
        }
        // checkpointせずにbackup
        let backup_dir = tmp.path().join("db-backups");
        let dest = create_backup(&conn, &backup_dir, 3, 1).unwrap();
        let bk = Connection::open(&dest).unwrap();
        let n: i64 = bk
            .query_row("SELECT count(*) FROM library_roots", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 50, "WALの未checkpoint分もbackupに含まれる");
        // busy_timeoutが動作すること (接続レベルAPIの互換確認)
        conn.busy_timeout(Duration::from_millis(100)).unwrap();
    }

    fn list_backups(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .expect("backup dir exists")
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with(BACKUP_PREFIX) && n.ends_with(BACKUP_SUFFIX))
                    .unwrap_or(false)
            })
            .collect()
    }

    // ===== WP-18-1 D1: 全中間段階fixtureの最新版へのUpgrade実証 =====

    /// 段階 `stage` のschemaで成立する代表データを投入する。
    /// CHECK制約は段階ごとに異なるため、投入値は段階-aware:
    /// - `files.mtime_ns` は#4以降のみ存在
    /// - `file_operations.status` のCHECKは#5/#6で再構築される
    ///   (投入値は当該段階のCHECKに適合するものだけ)
    /// - `ai_profiles`/`ai_runs` は#8以降のみ存在
    /// - `jobs` は#8のフィルタ (未対応job_type・pending重複の破棄) で
    ///   意図的に除去されない行 (succeededかつ現行job_type) のみ投入。
    ///   破棄側の意味は `v7_populated_jobs_migrate_to_v8_with_legacy_and_duplicate_rows`
    ///   が正本。
    fn insert_stage_fixture(conn: &Connection, stage: i32) {
        conn.execute(
            "INSERT INTO library_roots (uuid, path, path_encoding, display_path, \
                 watch_enabled, ai_enabled, created_at, updated_at)
             VALUES ('00000000-0000-7000-8000-000000000180', x'2f77313831', 'utf8', \
                     '/w18-root', 1, 0, '2026-09-04T00:00:00.000Z', \
                     '2026-09-04T00:00:00.000Z')",
            [],
        )
        .unwrap();
        // assets ×2: 評価情報の代表として片方にrating=5・favorite=1を設定
        conn.execute(
            "INSERT INTO assets (uuid, content_hash, hash_algorithm, byte_size, width, height, \
                                    rating, triage, favorite, created_at, updated_at)
             VALUES ('00000000-0000-7000-8000-000000000101', 'blake3:w18a', 'blake3', 100, 32, \
                     32, 5, 'picked', 1, '2026-09-04T00:00:00.000Z', '2026-09-04T00:00:00.000Z'),
                    ('00000000-0000-7000-8000-000000000102', 'blake3:w18b', 'blake3', 200, 64, \
                     64, 0, 'unselected', 0, '2026-09-04T00:00:00.000Z', '2026-09-04T00:00:00.000Z')",
            [],
        )
        .unwrap();
        // files ×2 (mtime_nsは段階>=4のみ。それ以前は列自体が無い)
        let mtime_ns_col = if stage >= 4 { ", mtime_ns" } else { "" };
        let mtime_ns_val = if stage >= 4 { ", 1000000001" } else { "" };
        conn.execute(
            &format!(
                "INSERT INTO files (uuid, root_id, asset_id, rel_path, path_encoding, \
                                      display_name, extension, byte_size, mtime_ms, \
                                      scan_state, last_seen_at, created_at, updated_at \
                                      {mtime_ns_col})
                 VALUES ('00000000-0000-7000-8000-000000000111', 1, 1, x'612e706e67', 'utf8', \
                         'a.png', 'png', 100, 5000, 'present', '2026-09-04T00:00:00.000Z', \
                         '2026-09-04T00:00:00.000Z', '2026-09-04T00:00:00.000Z' {mtime_ns_val}),
                        ('00000000-0000-7000-8000-000000000112', 1, 2, x'622e706e67', 'utf8', \
                         'b.png', 'png', 200, 6000, 'present', '2026-09-04T00:00:00.000Z', \
                         '2026-09-04T00:00:00.000Z', '2026-09-04T00:00:00.000Z' {mtime_ns_val})"
            ),
            [],
        )
        .unwrap();
        // tags ×2 + asset_tags ×1 + captions ×1 (現行)
        conn.execute(
            "INSERT INTO tags (uuid, display_name, normalized_name, category, created_at, updated_at)
             VALUES ('00000000-0000-7000-8000-000000000121', '風景', '風景', NULL, \
                     '2026-09-04T00:00:00.000Z', '2026-09-04T00:00:00.000Z'),
                    ('00000000-0000-7000-8000-000000000122', 'ポートレート', 'ポートレート', \
                     NULL, '2026-09-04T00:00:00.000Z', '2026-09-04T00:00:00.000Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO asset_tags (asset_id, tag_id, source, manually_edited, created_at)
             VALUES (1, 1, 'manual', 1, '2026-09-04T00:00:00.000Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO captions (uuid, asset_id, kind, body, source, current, created_at, updated_at)
             VALUES ('00000000-0000-7000-8000-000000000131', 1, 'description', \
                     'wp18 fixture caption', 'manual', 1, '2026-09-04T00:00:00.000Z', \
                     '2026-09-04T00:00:00.000Z')",
            [],
        )
        .unwrap();
        // collections (manual) + items + asset_metadata + embeddings
        conn.execute(
            "INSERT INTO collections (uuid, name, kind, created_at, updated_at)
             VALUES ('00000000-0000-7000-8000-000000000141', 'wp18選', 'manual', \
                     '2026-09-04T00:00:00.000Z', '2026-09-04T00:00:00.000Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO collection_items (collection_id, asset_id, position, note, created_at)
             VALUES (1, 1, 7, 'wp18 note', '2026-09-04T00:00:00.000Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO asset_metadata (uuid, asset_id, camera_make, camera_model, \
                                            parser_name, parser_version, created_at, updated_at)
             VALUES ('00000000-0000-7000-8000-000000000151', 1, 'wp18-camera', 'wp18-model', \
                     'exif', '1', '2026-09-04T00:00:00.000Z', '2026-09-04T00:00:00.000Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO embeddings (uuid, asset_id, purpose, model_name, model_revision, \
                                        dimensions, dtype, source_hash, vector, created_at)
             VALUES ('00000000-0000-7000-8000-000000000161', 1, 'caption', 'ruri-v3-70m', '1', \
                     2, 'f32', 'w18h', x'0100000002000000', '2026-09-04T00:00:00.000Z')",
            [],
        )
        .unwrap();
        // jobs: 全段階のCHECKを満たしv8/v9フィルタで存続する行
        conn.execute(
            "INSERT INTO jobs (uuid, job_type, target_type, target_id, status, priority, \
                                  payload, created_at, updated_at)
             VALUES ('00000000-0000-7000-8000-000000000181', 'ai_caption_tags', 'asset', 1, \
                     'succeeded', 5, '{}', '2026-09-04T00:00:00.000Z', \
                     '2026-09-04T00:00:00.000Z')",
            [],
        )
        .unwrap();
        // file_operations: 段階のCHECKに適合するstatus値 (検証側で意味的等価を判定)
        let (op_a, op_b) = if stage <= 4 {
            ("'pending'", "'succeeded'") // #5のCHECK前の旧値
        } else if stage == 5 {
            ("'planned'", "'done'")
        } else {
            ("'planned'", "'partial_undone'")
        };
        conn.execute(
            &format!(
                "INSERT INTO file_operations (uuid, operation_type, status, undoable, \
                                                  request_json, created_at)
                 VALUES ('00000000-0000-7000-8000-000000000182', 'copy', {op_a}, 1, '{{}}', \
                         '2026-09-04T00:00:00.000Z'),
                        ('00000000-0000-7000-8000-000000000183', 'move', {op_b}, 1, '{{}}', \
                         '2026-09-04T00:00:00.000Z')"
            ),
            [],
        )
        .unwrap();
        // ai_profiles / ai_runs: #8以降のみ
        if stage >= 8 {
            conn.execute(
                "INSERT INTO ai_profiles (uuid, display_name, base_url, model, vision_capable, \
                                              created_at, updated_at)
                 VALUES ('00000000-0000-7000-8000-000000000191', 'wp18-profile', \
                         'http://127.0.0.1:8787/v1', 'wp18-model', 1, \
                         '2026-09-04T00:00:00.000Z', '2026-09-04T00:00:00.000Z')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO ai_runs (uuid, asset_id, profile_id, operation, status, \
                                         prompt_version, started_at, created_at)
                 VALUES ('00000000-0000-7000-8000-000000000192', 1, 1, 'caption_and_tags', \
                         'succeeded', 1, '2026-09-04T00:00:00.000Z', '2026-09-04T00:00:00.000Z')",
                [],
            )
            .unwrap();
        }
    }

    /// 全段階fixture検証の本体 (下のテストから呼ばれる)。
    fn run_stage_fixture_case(stage: i32) {
        let tmp = TempDir::new(&format!("wp18s{stage}"));
        let backup_dir = tmp.path().join("db-backups");
        let db_path = tmp.path().join("db.sqlite3");

        // (1) 段階DB構築 + 代表データ投入 (FKを厳密に検証するためON)
        let mut conn = Connection::open(&db_path).unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        run_migrations(
            &mut conn,
            &MIGRATIONS[..stage as usize],
            &MigrationOpts::default(),
        )
        .unwrap();
        assert_eq!(read_user_version(&conn).unwrap(), stage);
        insert_stage_fixture(&conn, stage);

        // (2) 最新まで適用 (backup付き — データ保有DBのため必須)
        let opts = MigrationOpts {
            backup_dir: Some(backup_dir.clone()),
            keep_generations: 3,
        };
        let outcome = run_migrations(&mut conn, MIGRATIONS, &opts).unwrap();
        let expected: Vec<i32> = ((stage + 1)..=LATEST_VERSION).collect();
        assert_eq!(outcome.applied, expected, "段階{stage}からの適用列");
        assert_eq!(read_user_version(&conn).unwrap(), LATEST_VERSION);
        let backup = outcome.backup.expect("データ保有DBはbackup作成");
        assert!(backup.starts_with(&backup_dir));

        // (3) backupは段階{stage}のスナップショットとして単独で開ける
        let bk = Connection::open(&backup).unwrap();
        assert_eq!(
            bk.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            stage as i64
        );
        drop(bk);

        // (4) データ保持の検証は段階ごとの意味変換を踏まえて行う
        verify_stage_fixture_after_upgrade(&conn, stage);

        // (5) 整合性: FK違反なし・integrity ok
        let fk_violations: i64 = conn
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(fk_violations, 0, "段階{stage}: FK違反");
        let ic: String = conn
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ic, "ok", "段階{stage}: integrity");
    }

    /// 最新版適用後の代表データ検証 (段階awareな意味的等価)。
    fn verify_stage_fixture_after_upgrade(conn: &Connection, stage: i32) {
        // library_roots: 恒等
        let (root_count, display): (i64, String) = conn
            .query_row(
                "SELECT count(*), max(display_path) FROM library_roots",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (root_count, display.as_str()),
            (1, "/w18-root"),
            "段階{stage}"
        );

        // assets: 恒等 (評価・選別・お気に入りは評価情報の代表)
        let (assets, rating, triage, favorite): (i64, i64, String, i64) = conn
            .query_row(
                "SELECT count(*), max(rating), max(triage), max(favorite) FROM assets",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!((assets, rating, favorite), (2, 5, 1), "段階{stage}");
        assert_eq!(triage, "unselected", "段階{stage}: max()は辞書順");

        // files: mtime_nsは段階4以降の投入値を保持。それ以前はNULL
        // (次の走査で充填 — 既存v3to4テストと同じ後方互換)。
        let (files, ns): (i64, Option<i64>) = conn
            .query_row("SELECT count(*), max(mtime_ns) FROM files", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(files, 2, "段階{stage}");
        assert_eq!(
            ns,
            if stage >= 4 {
                Some(1_000_000_001)
            } else {
                None
            },
            "段階{stage}: mtime_ns"
        );

        // tags / asset_tags / captions: 恒等
        let tags: i64 = conn
            .query_row("SELECT count(*) FROM tags", [], |r| r.get(0))
            .unwrap();
        assert_eq!(tags, 2, "段階{stage}");
        let (at, at_source): (i64, String) = conn
            .query_row("SELECT count(*), max(source) FROM asset_tags", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((at, at_source.as_str()), (1, "manual"), "段階{stage}");
        let (caps, body): (i64, String) = conn
            .query_row(
                "SELECT count(*), max(body) FROM captions WHERE current = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (caps, body.as_str()),
            (1, "wp18 fixture caption"),
            "段階{stage}"
        );

        // collections / collection_items / asset_metadata / embeddings: 恒等
        let colls: i64 = conn
            .query_row("SELECT count(*) FROM collections", [], |r| r.get(0))
            .unwrap();
        assert_eq!(colls, 1, "段階{stage}");
        let (items, pos): (i64, i64) = conn
            .query_row(
                "SELECT count(*), max(position) FROM collection_items",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((items, pos), (1, 7), "段階{stage}");
        let make: Option<String> = conn
            .query_row("SELECT camera_make FROM asset_metadata", [], |r| r.get(0))
            .unwrap();
        assert_eq!(make.as_deref(), Some("wp18-camera"), "段階{stage}");
        let vector: Vec<u8> = conn
            .query_row("SELECT vector FROM embeddings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(vector, vec![1u8, 0, 0, 0, 2, 0, 0, 0], "段階{stage}");

        // jobs: 存続対象の行がuuidごと保持される
        let jobs: i64 = conn
            .query_row(
                "SELECT count(*) FROM jobs WHERE uuid = '00000000-0000-7000-8000-000000000181'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(jobs, 1, "段階{stage}: 存続jobの保持");

        // file_operations: 段階<=4の旧statusは#5の文書化マッピング
        // (pending→executing, succeeded→done) が「論理等価」の定義。
        // 段階5は#5のCHECK値・段階>=6は#6のCHECK値 = 恒等コピー。
        let statuses: Vec<(String, String)> = {
            let mut stmt = conn
                .prepare("SELECT uuid, status FROM file_operations ORDER BY uuid")
                .unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        let expected_ops: Vec<(&str, &str)> = if stage <= 4 {
            vec![
                ("00000000-0000-7000-8000-000000000182", "executing"),
                ("00000000-0000-7000-8000-000000000183", "done"),
            ]
        } else if stage == 5 {
            vec![
                ("00000000-0000-7000-8000-000000000182", "planned"),
                ("00000000-0000-7000-8000-000000000183", "done"),
            ]
        } else {
            vec![
                ("00000000-0000-7000-8000-000000000182", "planned"),
                ("00000000-0000-7000-8000-000000000183", "partial_undone"),
            ]
        };
        let got: Vec<(String, String)> = expected_ops
            .iter()
            .map(|(uuid, _)| {
                statuses
                    .iter()
                    .find(|(u, _)| u == uuid)
                    .unwrap_or_else(|| panic!("段階{stage}: file_operation {uuid} が消失"))
                    .clone()
            })
            .collect();
        for ((_, want), (_, got_status)) in expected_ops.iter().zip(got.iter()) {
            assert_eq!(got_status, want, "段階{stage}: status mapping");
        }
        assert_eq!(statuses.len(), 2, "段階{stage}: file_operations行数");

        // ai_profiles / ai_runs (#8以降の投入分): v9は影響しないため恒等
        if stage >= 8 {
            let (profiles, runs, profile_name): (i64, i64, String) = conn
                .query_row(
                    "SELECT (SELECT count(*) FROM ai_profiles), \
                            (SELECT count(*) FROM ai_runs), \
                            (SELECT max(display_name) FROM ai_profiles)",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            assert_eq!(
                (profiles, runs, profile_name.as_str()),
                (1, 1, "wp18-profile"),
                "段階{stage}"
            );
        }
    }

    #[test]
    fn every_stage_fixture_upgrades_to_latest_with_data_retention() {
        // 初期→v1→...→v8の各段階で代表データを投入したDBが、最新 (v9) へ
        // (a) 適用成功 (b) データ保持 (段階固有の文書化変換を含む論理等価)
        // で移行できること的全数実証 (§26「全migrationを新規DBと旧fixtureで
        // 検証する」の旧fixture側。新規DB側はfresh_db系テストが既に網羅)。
        // v9は最新そのため段階ループに含めない (冪等は
        // already_migrated_db_is_noopが検証済み)。
        for stage in 1..LATEST_VERSION {
            run_stage_fixture_case(stage);
        }
    }

    #[test]
    fn stage8_fixture_is_noop_at_latest_and_retains_data() {
        // 最新段階 (=適用済み) のデータ保有DBは再実行で何も起きない
        // (アップグレード経路の終端: 適用列空・backup無し・データ不変)。
        let tmp = TempDir::new("wp18s9");
        let mut conn = Connection::open(tmp.path().join("db.sqlite3")).unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        run_migrations(&mut conn, MIGRATIONS, &MigrationOpts::default()).unwrap();
        insert_stage_fixture(&conn, LATEST_VERSION);
        let outcome = run_migrations(&mut conn, MIGRATIONS, &MigrationOpts::default()).unwrap();
        assert!(outcome.applied.is_empty());
        assert_eq!(outcome.backup, None);
        verify_stage_fixture_after_upgrade(&conn, LATEST_VERSION);
    }
}
