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

    // 検証: 読み取り専用で開いてintegrity_check。失敗なら移行を止める。
    let check = Connection::open_with_flags(&dest, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
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

    fn failing_migration_v5() -> Migration {
        Migration {
            version: 5,
            name: "test_failing",
            sql: "CREATE TABLE wp1_test_extra (id INTEGER PRIMARY KEY); INSERT INTO wp1_test_extra VALUES (1); INSERT INTO nonexistent_table VALUES (1);",
        }
    }

    fn ok_migration_v5() -> Migration {
        Migration {
            version: 5,
            name: "test_ok_extra",
            sql: "CREATE TABLE wp1_test_extra (id INTEGER PRIMARY KEY);",
        }
    }

    #[test]
    fn fresh_db_creates_latest_schema_and_sets_user_version() {
        let tmp = TempDir::new("fresh");
        let mut conn = Connection::open(tmp.path().join("db.sqlite3")).unwrap();
        let outcome = run_migrations(&mut conn, MIGRATIONS, &MigrationOpts::default()).unwrap();
        assert_eq!(outcome.applied, vec![1, 2, 3, 4]);
        assert_eq!(outcome.backup, None); // 新規DB (v0) はbackup不要
        assert_eq!(read_user_version(&conn).unwrap(), LATEST_VERSION);

        // 12テーブル + 主要インデックスが存在する
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
        assert_eq!(outcome.applied, vec![3, 4]);
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
        assert_eq!(outcome.applied, vec![4]);
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
        list.push(failing_migration_v5());
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
                "SELECT count(*) > 0 FROM sqlite_master WHERE name='wp1_test_extra'",
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
        list_ok.push(ok_migration_v5());
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
        list.push(ok_migration_v5());
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
        list.push(ok_migration_v5());
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
    fn migration_v1_defers_ai_run_id_columns_to_wp10() {
        // FKなしのai_run_id列を先行定義すると、後のFK付与にはテーブル再構築が
        // 必要になる (SQLiteは既存列へのFK追加ができない)。WP-10で
        // ai_runsテーブル作成と同時にADD COLUMNで追加するため、#1には含めない。
        // 検証はPRAGMA table_info (実列) で行う — sqlite_masterのSQL文本体には
        // 経緯コメントが残り得るため。
        let tmp = TempDir::new("airunid");
        let mut conn = Connection::open(tmp.path().join("db.sqlite3")).unwrap();
        run_migrations(&mut conn, MIGRATIONS, &MigrationOpts::default()).unwrap();
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
}
