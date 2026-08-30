//! DB整合性検査・診断 (MVP WP-1)。
//!
//! `PRAGMA integrity_check` / `PRAGMA foreign_key_check` / schema version /
//! journal_mode / テーブル行数 / WALサイズを1スナップショットで返す。
//! Tauri command `db_status` (apps/desktop/src-tauri/src/db.rs) 経由で
//! 最小公開する。

use std::collections::BTreeMap;

use rusqlite::Connection;
use serde::Serialize;

use crate::{Db, DbError};

/// 診断スナップショット。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DbStatus {
    /// DBファイルのパス (表示用文字列)。
    pub path: String,
    pub schema_version: i32,
    pub journal_mode: String,
    /// `ok` または問題の要約。
    pub integrity_check: String,
    /// `PRAGMA foreign_key_check` の違反行数。
    pub foreign_key_violations: u64,
    /// テーブル名 → 行数 (12テーブル固定、名前順)。
    pub table_row_counts: BTreeMap<String, u64>,
    /// DB本体のファイルサイズ (バイト)。
    pub db_bytes: u64,
    /// `-wal` ファイルのサイズ (バイト。checkpoint済みでファイル不在なら0)。
    pub wal_bytes: u64,
    pub busy_timeout_ms: u64,
}

impl Db {
    /// 整合性検査・診断を実行する。読み取りのみ (writerを止めない)。
    pub fn diagnose(&self) -> Result<DbStatus, DbError> {
        self.with_reader(|repos| {
            collect_status(repos.conn(), &self.path, self.options.busy_timeout_ms)
        })
        .map_err(|e| DbError::Query(e.to_string()))
    }
}

fn collect_status(
    conn: &Connection,
    path: &std::path::Path,
    busy_timeout_ms: u64,
) -> Result<DbStatus, gento_core::RepoError> {
    let sql_err = |e: rusqlite::Error| gento_core::RepoError::Db(e.to_string());
    let schema_version: i32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(sql_err)?;
    let journal_mode: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .map_err(sql_err)?;
    let integrity_check: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .map_err(sql_err)?;

    let mut fk_stmt = conn.prepare("PRAGMA foreign_key_check").map_err(sql_err)?;
    let mut rows = fk_stmt.query([]).map_err(sql_err)?;
    let mut foreign_key_violations: u64 = 0;
    while rows.next().map_err(sql_err)?.is_some() {
        foreign_key_violations += 1;
    }

    let mut table_row_counts = BTreeMap::new();
    for table in crate::schema::TABLES {
        let n: u64 = conn
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .map_err(sql_err)?;
        table_row_counts.insert(table.to_string(), n);
    }

    let db_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let wal_path = {
        // SQLiteのWALファイル名規約: <db>-wal
        let mut s = path.as_os_str().to_os_string();
        s.push("-wal");
        std::path::PathBuf::from(s)
    };
    let wal_bytes = std::fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0);

    Ok(DbStatus {
        path: path.display().to_string(),
        schema_version,
        journal_mode,
        integrity_check,
        foreign_key_violations,
        table_row_counts,
        db_bytes,
        wal_bytes,
        busy_timeout_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DbOptions;
    use gento_core::repo::{FileRepo, FileUpsert, LibraryRootRepo, ScanState};

    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "gento-db-diag-{tag}-{}-{}",
                std::process::id(),
                crate::ids::now_utc_compact()
            ));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn diagnose_fresh_db_is_healthy() {
        let tmp = TempDir::new("fresh");
        let path = tmp.0.join("db.sqlite3");
        let db = Db::open_with(&path, DbOptions::default()).unwrap();
        let status = db.diagnose().unwrap();

        assert_eq!(status.path, path.display().to_string());
        assert_eq!(status.schema_version, crate::schema::LATEST_VERSION);
        assert_eq!(status.journal_mode, "wal");
        assert_eq!(status.integrity_check, "ok");
        assert_eq!(status.foreign_key_violations, 0);
        assert_eq!(status.table_row_counts.len(), 12);
        assert!(status.table_row_counts.contains_key("library_roots"));
        assert!(status.table_row_counts.values().all(|&n| n == 0));
        assert!(status.db_bytes > 0);
        assert_eq!(status.busy_timeout_ms, DbOptions::default().busy_timeout_ms);
    }

    #[test]
    fn diagnose_counts_rows_and_wal_grows() {
        let tmp = TempDir::new("rows");
        let path = tmp.0.join("db.sqlite3");
        let db = Db::open_with(&path, DbOptions::default()).unwrap();
        db.with_writer(|repos| {
            let root = repos.add_root(std::path::Path::new("/data/p"))?;
            repos.upsert_file(FileUpsert {
                root_id: root.id,
                rel_path: std::path::PathBuf::from("a.png"),
                display_name: "a.png".to_string(),
                byte_size: 1,
                mtime_ms: 1,
                scan_state: ScanState::Present,
            })?;
            Ok(())
        })
        .unwrap();

        let status = db.diagnose().unwrap();
        assert_eq!(status.table_row_counts["library_roots"], 1);
        assert_eq!(status.table_row_counts["files"], 1);
        assert!(status.wal_bytes > 0, "書き込み後にWALが存在する");
    }

    #[test]
    fn db_status_serializes_for_ipc() {
        let tmp = TempDir::new("json");
        let db = Db::open_with(tmp.0.join("db.sqlite3"), DbOptions::default()).unwrap();
        let status = db.diagnose().unwrap();
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["schema_version"], serde_json::json!(1));
        assert_eq!(json["journal_mode"], serde_json::json!("wal"));
        assert_eq!(json["integrity_check"], serde_json::json!("ok"));
        assert!(json["table_row_counts"].is_object());
        assert_eq!(json["foreign_key_violations"], serde_json::json!(0));
    }
}
