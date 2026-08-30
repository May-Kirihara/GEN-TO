//! repository traitのSQLite実装 (MVP WP-1)。
//!
//! [`SqliteRepos`] は1つの[`rusqlite::Connection`]の上に
//! [`gento_core::repo`]の3 trait (LibraryRootRepo / FileRepo / AssetRepo)
//! を実装する。トランザクション境界は呼び出し側
//! ([`crate::Db::with_writer`] 等)が持つ — 各メソッドは単文atomic。

use std::path::Path;

use gento_core::repo::{
    AssetRecord, AssetRepo, FileRecord, FileRepo, FileUpsert, HashState, LibraryRootRecord,
    LibraryRootRepo, NewAsset, RepoError, ScanState, TriageState,
};
use rusqlite::{params, Connection, OptionalExtension};

use crate::ids::{new_uuid_v7, now_utc_rfc3339};
use crate::paths::{decode_path, encode_path, PathEncoding};

/// 1接続ぶんのrepository集合。
pub struct SqliteRepos<'a> {
    conn: &'a Connection,
}

impl<'a> SqliteRepos<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// 高度な操作 (diagnose・batch等) 用の接続参照。
    pub fn conn(&self) -> &Connection {
        self.conn
    }

    fn get_root(&self, id: i64) -> Result<Option<LibraryRootRecord>, RepoError> {
        self.conn
            .query_row(
                "SELECT id, uuid, path, path_encoding, display_path, enabled, created_at, updated_at
                 FROM library_roots WHERE id = ?1",
                [id],
                map_library_root,
            )
            .optional()
            .map_err(map_err)
    }
}

/// rusqliteエラー → [`RepoError`]。
/// - 一意制約違反 (UNIQUE/PK) → [`RepoError::Duplicate`]
/// - CHECK・NOT NULL等、一意・FK以外の制約違反 → [`RepoError::Constraint`]
///   (Duplicateと誤分類すると呼び出し側が「重複」処理をしてしまうため区別)
/// - 外部キー違反 → 「foreign key violation」付きの [`RepoError::Db`]
fn map_err(e: rusqlite::Error) -> RepoError {
    if let rusqlite::Error::SqliteFailure(code, msg) = &e {
        if code.code == rusqlite::ffi::ErrorCode::ConstraintViolation {
            let is_fk = code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY
                || msg.as_deref().is_some_and(|m| m.contains("FOREIGN KEY"));
            if is_fk {
                return RepoError::Db(format!(
                    "foreign key violation: {}",
                    msg.as_deref().unwrap_or_default()
                ));
            }
            let is_unique = code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
                || code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY
                || msg
                    .as_deref()
                    .is_some_and(|m| m.contains("UNIQUE") || m.contains("PRIMARY KEY"));
            if is_unique {
                return RepoError::Duplicate;
            }
            return RepoError::Constraint(
                msg.as_deref()
                    .unwrap_or("check or not-null violation")
                    .to_string(),
            );
        }
    }
    RepoError::Db(e.to_string())
}

/// u64値をi64列へ格納する。直接castはオーバーフローで負値にラップする
/// ため禁止し、範囲外はErrとする。
fn u64_to_i64(value: u64, field: &'static str) -> Result<i64, RepoError> {
    i64::try_from(value).map_err(|_| RepoError::Db(format!("{field} exceeds i64 range: {value}")))
}

fn map_library_root(row: &rusqlite::Row<'_>) -> rusqlite::Result<LibraryRootRecord> {
    let path_bytes: Vec<u8> = row.get("path")?;
    let encoding: String = row.get("path_encoding")?;
    // 格納値はCHECK制約で保証済み。壊れた値は実装エラーとして扱う
    let enc = PathEncoding::from_db_value(&encoding).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            format!("unknown path_encoding: {encoding}").into(),
        )
    })?;
    let _ = enc; // decodeはUnixではタグ非依存 (将来のWindows正準化で使用)
    Ok(LibraryRootRecord {
        id: row.get("id")?,
        uuid: row.get("uuid")?,
        path: decode_path(&path_bytes),
        display_path: row.get("display_path")?,
        enabled: row.get::<_, i64>("enabled")? != 0,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

fn map_file(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileRecord> {
    let rel_bytes: Vec<u8> = row.get("rel_path")?;
    let scan_state_str: String = row.get("scan_state")?;
    let scan_state = scan_state_from_db_value(&scan_state_str)?;
    Ok(FileRecord {
        id: row.get("id")?,
        uuid: row.get("uuid")?,
        root_id: row.get("root_id")?,
        asset_id: row.get("asset_id")?,
        rel_path: decode_path(&rel_bytes),
        display_name: row.get("display_name")?,
        extension: row.get("extension")?,
        byte_size: row.get::<_, i64>("byte_size")?.max(0) as u64,
        mtime_ms: row.get("mtime_ms")?,
        scan_state,
        last_seen_at: row.get("last_seen_at")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

fn scan_state_from_db_value(s: &str) -> rusqlite::Result<ScanState> {
    use std::str::FromStr;
    ScanState::from_str(s).map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            format!("unknown scan_state: {s}").into(),
        )
    })
}

fn map_asset(row: &rusqlite::Row<'_>) -> rusqlite::Result<AssetRecord> {
    let triage_str: String = row.get("triage")?;
    let hash_state_str: String = row.get("hash_state")?;
    let triage = match triage_str.as_str() {
        "unselected" => TriageState::Unselected,
        "picked" => TriageState::Picked,
        "rejected" => TriageState::Rejected,
        other => {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                format!("unknown triage: {other}").into(),
            ))
        }
    };
    let hash_state = match hash_state_str.as_str() {
        "pending" => HashState::Pending,
        "done" => HashState::Done,
        other => {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                format!("unknown hash_state: {other}").into(),
            ))
        }
    };
    Ok(AssetRecord {
        id: row.get("id")?,
        uuid: row.get("uuid")?,
        content_hash: row.get("content_hash")?,
        hash_algorithm: row.get("hash_algorithm")?,
        hash_state,
        byte_size: row.get::<_, i64>("byte_size")?.max(0) as u64,
        mime_type: row.get("mime_type")?,
        rating: row.get::<_, i64>("rating")?.clamp(0, 5) as u8,
        triage,
        favorite: row.get::<_, i64>("favorite")? != 0,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

/// 表示名から拡張子 (小文字) を導出する。dotfile (`.bashrc` 等) は
/// 拡張子なしと扱う。
fn extension_of(display_name: &str) -> Option<String> {
    display_name
        .rsplit_once('.')
        .filter(|(head, _)| !head.is_empty())
        .map(|(_, ext)| ext.to_ascii_lowercase())
}

impl LibraryRootRepo for SqliteRepos<'_> {
    fn add_root(&self, path: &Path) -> Result<LibraryRootRecord, RepoError> {
        let (blob, enc) = encode_path(path);
        let display = path.to_string_lossy().into_owned();
        let now = now_utc_rfc3339();
        self.conn
            .execute(
                "INSERT INTO library_roots
                    (uuid, path, path_encoding, display_path, enabled, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 1, ?5, ?5)",
                params![new_uuid_v7(), blob, enc.as_str(), display, now],
            )
            .map_err(map_err)?;
        let id = self.conn.last_insert_rowid();
        self.get_root(id)?
            .ok_or_else(|| RepoError::Db("inserted library_root row is not readable".to_string()))
    }

    fn list_roots(&self) -> Result<Vec<LibraryRootRecord>, RepoError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, uuid, path, path_encoding, display_path, enabled, created_at, updated_at
                 FROM library_roots ORDER BY id",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], map_library_root)
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    fn set_root_enabled(&self, root_id: i64, enabled: bool) -> Result<(), RepoError> {
        let changed = self
            .conn
            .execute(
                "UPDATE library_roots SET enabled = ?2, updated_at = ?3 WHERE id = ?1",
                params![root_id, enabled as i64, now_utc_rfc3339()],
            )
            .map_err(map_err)?;
        if changed == 0 {
            return Err(RepoError::NotFound);
        }
        Ok(())
    }

    fn remove_root(&self, root_id: i64) -> Result<(), RepoError> {
        let changed = self
            .conn
            .execute("DELETE FROM library_roots WHERE id = ?1", [root_id])
            .map_err(map_err)?;
        if changed == 0 {
            return Err(RepoError::NotFound);
        }
        Ok(())
    }

    fn overlapping_roots(&self, path: &Path) -> Result<Vec<LibraryRootRecord>, RepoError> {
        use gento_core::repo::path_is_within;
        Ok(self
            .list_roots()?
            .into_iter()
            .filter(|root| path_is_within(path, &root.path) || path_is_within(&root.path, path))
            .collect())
    }
}

impl FileRepo for SqliteRepos<'_> {
    fn upsert_file(&self, file: FileUpsert) -> Result<FileRecord, RepoError> {
        let (rel_blob, enc) = encode_path(&file.rel_path);
        let now = now_utc_rfc3339();
        let extension = extension_of(&file.display_name);
        self.conn
            .execute(
                "INSERT INTO files
                    (uuid, root_id, asset_id, rel_path, path_encoding, display_name,
                     extension, byte_size, mtime_ms, scan_state, last_seen_at,
                     created_at, updated_at)
                 VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?10)
                 ON CONFLICT (root_id, path_encoding, rel_path) DO UPDATE SET
                    display_name = excluded.display_name,
                    extension = excluded.extension,
                    byte_size = excluded.byte_size,
                    mtime_ms = excluded.mtime_ms,
                    scan_state = excluded.scan_state,
                    last_seen_at = excluded.last_seen_at,
                    updated_at = excluded.updated_at",
                params![
                    new_uuid_v7(),
                    file.root_id,
                    rel_blob,
                    enc.as_str(),
                    file.display_name,
                    extension,
                    u64_to_i64(file.byte_size, "byte_size")?,
                    u64_to_i64(file.mtime_ms, "mtime_ms")?,
                    file.scan_state.as_str(),
                    now,
                ],
            )
            .map_err(map_err)?;

        // upsert後の行を引き直す (root不在はFK違反で上のINSERTが失敗済み)
        self.find_by_path(file.root_id, &file.rel_path)?
            .ok_or_else(|| RepoError::Db("upserted file row is not readable".to_string()))
    }

    fn set_scan_state(&self, file_id: i64, state: ScanState) -> Result<(), RepoError> {
        let changed = self
            .conn
            .execute(
                "UPDATE files SET scan_state = ?2, updated_at = ?3 WHERE id = ?1",
                params![file_id, state.as_str(), now_utc_rfc3339()],
            )
            .map_err(map_err)?;
        if changed == 0 {
            return Err(RepoError::NotFound);
        }
        Ok(())
    }

    fn list_missing(&self, root_id: i64) -> Result<Vec<FileRecord>, RepoError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, uuid, root_id, asset_id, rel_path, path_encoding, display_name,
                        extension, byte_size, mtime_ms, scan_state, last_seen_at,
                        created_at, updated_at
                 FROM files WHERE root_id = ?1 AND scan_state = 'missing' ORDER BY id",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map([root_id], map_file)
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    fn find_by_path(&self, root_id: i64, rel_path: &Path) -> Result<Option<FileRecord>, RepoError> {
        let (rel_blob, enc) = encode_path(rel_path);
        self.conn
            .query_row(
                "SELECT id, uuid, root_id, asset_id, rel_path, path_encoding, display_name,
                        extension, byte_size, mtime_ms, scan_state, last_seen_at,
                        created_at, updated_at
                 FROM files WHERE root_id = ?1 AND path_encoding = ?2 AND rel_path = ?3",
                params![root_id, enc.as_str(), rel_blob],
                map_file,
            )
            .optional()
            .map_err(map_err)
    }
}

impl AssetRepo for SqliteRepos<'_> {
    fn create_asset(&self, asset: NewAsset) -> Result<AssetRecord, RepoError> {
        let now = now_utc_rfc3339();
        self.conn
            .execute(
                "INSERT INTO assets
                    (uuid, content_hash, hash_algorithm, hash_state, mime_type,
                     byte_size, rating, triage, favorite, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 'done', ?4, ?5, 0, 'unselected', 0, ?6, ?6)",
                params![
                    new_uuid_v7(),
                    asset.content_hash,
                    asset.hash_algorithm,
                    asset.mime_type,
                    u64_to_i64(asset.byte_size, "byte_size")?,
                    now,
                ],
            )
            .map_err(map_err)?;
        let id = self.conn.last_insert_rowid();
        self.conn
            .query_row(
                "SELECT id, uuid, content_hash, hash_algorithm, hash_state, mime_type,
                        byte_size, rating, triage, favorite, created_at, updated_at
                 FROM assets WHERE id = ?1",
                [id],
                map_asset,
            )
            .map_err(map_err)
    }

    fn find_by_hash(&self, content_hash: &str) -> Result<Option<AssetRecord>, RepoError> {
        self.conn
            .query_row(
                "SELECT id, uuid, content_hash, hash_algorithm, hash_state, mime_type,
                        byte_size, rating, triage, favorite, created_at, updated_at
                 FROM assets WHERE content_hash = ?1",
                [content_hash],
                map_asset,
            )
            .optional()
            .map_err(map_err)
    }

    fn link_file(&self, file_id: i64, asset_id: i64) -> Result<(), RepoError> {
        // 先に双方の存在を確認し、「どちらが無いか」をNotFoundで返せるようにする
        let asset_exists: Option<i64> = self
            .conn
            .query_row("SELECT id FROM assets WHERE id = ?1", [asset_id], |r| {
                r.get(0)
            })
            .optional()
            .map_err(map_err)?;
        if asset_exists.is_none() {
            return Err(RepoError::NotFound);
        }
        let changed = self
            .conn
            .execute(
                "UPDATE files SET asset_id = ?2, updated_at = ?3 WHERE id = ?1",
                params![file_id, asset_id, now_utc_rfc3339()],
            )
            .map_err(map_err)?;
        if changed == 0 {
            return Err(RepoError::NotFound);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrate::{run_migrations, MigrationOpts};
    use crate::schema::MIGRATIONS;
    use gento_core::repo::path_is_within;
    use std::path::PathBuf;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "gento-db-repos-{tag}-{}-{}",
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

    /// マイグレーション済みDB上の単一接続でrepoを試すヘルパ。
    fn with_repos<T>(tag: &str, f: impl FnOnce(&SqliteRepos<'_>) -> T) -> T {
        let tmp = TempDir::new(tag);
        let mut conn = Connection::open(tmp.0.join("db.sqlite3")).unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        run_migrations(&mut conn, MIGRATIONS, &MigrationOpts::default()).unwrap();
        let repos = SqliteRepos::new(&conn);
        f(&repos)
    }

    // ===== LibraryRootRepo =====

    #[test]
    fn add_and_list_roots() {
        with_repos("roots", |repos| {
            let r1 = repos.add_root(Path::new("/data/photos")).unwrap();
            let r2 = repos.add_root(Path::new("/data/other")).unwrap();
            assert_ne!(r1.id, r2.id);
            assert_eq!(r1.path, PathBuf::from("/data/photos"));
            assert_eq!(r1.display_path, "/data/photos");
            assert!(r1.enabled);
            assert_eq!(r1.uuid.len(), 36);
            assert!(r1.created_at.ends_with('Z'));

            let list = repos.list_roots().unwrap();
            assert_eq!(list.len(), 2);
            assert_eq!(list[0].id, r1.id);
            assert_eq!(list[1].path, PathBuf::from("/data/other"));
        });
    }

    #[test]
    fn duplicate_root_path_is_rejected() {
        with_repos("dup", |repos| {
            repos.add_root(Path::new("/data/photos")).unwrap();
            let err = repos.add_root(Path::new("/data/photos")).unwrap_err();
            assert_eq!(err, RepoError::Duplicate);
        });
    }

    #[test]
    fn set_enabled_and_remove_root() {
        with_repos("enable", |repos| {
            let root = repos.add_root(Path::new("/data/p")).unwrap();
            repos.set_root_enabled(root.id, false).unwrap();
            let list = repos.list_roots().unwrap();
            assert!(!list[0].enabled);

            // 不在ID
            assert_eq!(
                repos.set_root_enabled(9999, true).unwrap_err(),
                RepoError::NotFound
            );
            assert_eq!(repos.remove_root(9999).unwrap_err(), RepoError::NotFound);

            repos.remove_root(root.id).unwrap();
            assert!(repos.list_roots().unwrap().is_empty());
        });
    }

    #[test]
    fn remove_root_cascades_files() {
        with_repos("cascade", |repos| {
            let root = repos.add_root(Path::new("/data/p")).unwrap();
            let f = repos
                .upsert_file(FileUpsert {
                    root_id: root.id,
                    rel_path: PathBuf::from("a/b.png"),
                    display_name: "b.png".to_string(),
                    byte_size: 10,
                    mtime_ms: 1,
                    scan_state: ScanState::Present,
                })
                .unwrap();
            repos.remove_root(root.id).unwrap();
            let found = repos
                .find_by_path(root.id, &PathBuf::from("a/b.png"))
                .unwrap();
            assert!(found.is_none(), "root削除でfilesも削除 (CASCADE)");
            let _ = f;
        });
    }

    #[test]
    fn overlapping_roots_detects_parent_child() {
        with_repos("overlap", |repos| {
            repos.add_root(Path::new("/data/photos")).unwrap();
            repos.add_root(Path::new("/data/other")).unwrap();

            // 子を追加しようとする → 親が検出される
            let hits = repos
                .overlapping_roots(Path::new("/data/photos/sub"))
                .unwrap();
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].path, PathBuf::from("/data/photos"));

            // 親を追加しようとする → 既存子が検出される
            let hits = repos.overlapping_roots(Path::new("/data")).unwrap();
            assert_eq!(hits.len(), 2);

            // 無関係パス → 空
            assert!(repos
                .overlapping_roots(Path::new("/elsewhere"))
                .unwrap()
                .is_empty());

            // 同一パスも重複検出対象
            let hits = repos.overlapping_roots(Path::new("/data/other")).unwrap();
            assert_eq!(hits.len(), 1);
        });
    }

    // ===== FileRepo =====

    #[test]
    fn upsert_file_without_asset_and_update() {
        with_repos("file", |repos| {
            let root = repos.add_root(Path::new("/data/p")).unwrap();
            let f1 = repos
                .upsert_file(FileUpsert {
                    root_id: root.id,
                    rel_path: PathBuf::from("dir/img.png"),
                    display_name: "img.png".to_string(),
                    byte_size: 100,
                    mtime_ms: 1_700_000_000_000,
                    scan_state: ScanState::Present,
                })
                .unwrap();
            assert!(f1.asset_id.is_none(), "Asset作成前はNULL (受入条件)");
            assert_eq!(f1.extension.as_deref(), Some("png"));
            assert_eq!(f1.scan_state, ScanState::Present);

            // 同一キー再upsert → 更新 (uuid・asset_id不変)
            let f2 = repos
                .upsert_file(FileUpsert {
                    root_id: root.id,
                    rel_path: PathBuf::from("dir/img.png"),
                    display_name: "img.png".to_string(),
                    byte_size: 999,
                    mtime_ms: 1_700_000_999_000,
                    scan_state: ScanState::Missing,
                })
                .unwrap();
            assert_eq!(f2.id, f1.id);
            assert_eq!(f2.uuid, f1.uuid);
            assert_eq!(f2.byte_size, 999);
            assert_eq!(f2.scan_state, ScanState::Missing);

            // 異なるキー → 別行
            let f3 = repos
                .upsert_file(FileUpsert {
                    root_id: root.id,
                    rel_path: PathBuf::from("other.png"),
                    display_name: "other.png".to_string(),
                    byte_size: 1,
                    mtime_ms: 1,
                    scan_state: ScanState::Present,
                })
                .unwrap();
            assert_ne!(f3.id, f1.id);

            // 異なるrootの同一rel_pathも別行
            let root2 = repos.add_root(Path::new("/data/q")).unwrap();
            let f4 = repos
                .upsert_file(FileUpsert {
                    root_id: root2.id,
                    rel_path: PathBuf::from("dir/img.png"),
                    display_name: "img.png".to_string(),
                    byte_size: 5,
                    mtime_ms: 1,
                    scan_state: ScanState::Present,
                })
                .unwrap();
            assert_ne!(f4.id, f1.id);
        });
    }

    #[test]
    fn upsert_file_with_unknown_root_fails_by_fk() {
        with_repos("fkroot", |repos| {
            let err = repos
                .upsert_file(FileUpsert {
                    root_id: 424242,
                    rel_path: PathBuf::from("x.png"),
                    display_name: "x.png".to_string(),
                    byte_size: 1,
                    mtime_ms: 1,
                    scan_state: ScanState::Present,
                })
                .unwrap_err();
            match err {
                RepoError::Db(msg) => assert!(msg.contains("foreign key"), "{msg}"),
                other => panic!("expected FK error, got {other:?}"),
            }
        });
    }

    #[test]
    fn scan_state_update_and_missing_list() {
        with_repos("missing", |repos| {
            let root = repos.add_root(Path::new("/data/p")).unwrap();
            let mk = |rel: &str, state: ScanState| FileUpsert {
                root_id: root.id,
                rel_path: PathBuf::from(rel),
                display_name: rel.to_string(),
                byte_size: 1,
                mtime_ms: 1,
                scan_state: state,
            };
            let a = repos.upsert_file(mk("a.png", ScanState::Present)).unwrap();
            let b = repos.upsert_file(mk("b.png", ScanState::Missing)).unwrap();
            let _c = repos.upsert_file(mk("c.png", ScanState::Present)).unwrap();

            // missing一覧: bのみ
            let missing = repos.list_missing(root.id).unwrap();
            assert_eq!(missing.len(), 1);
            assert_eq!(missing[0].id, b.id);
            assert_eq!(missing[0].rel_path, PathBuf::from("b.png"));

            // aをmissingへ → 2件。missing は削除と区別された状態で保持される
            repos.set_scan_state(a.id, ScanState::Missing).unwrap();
            assert_eq!(repos.list_missing(root.id).unwrap().len(), 2);
            assert_eq!(
                repos.set_scan_state(9999, ScanState::Missing).unwrap_err(),
                RepoError::NotFound
            );

            // 再出現 → presentへ戻せる
            repos.set_scan_state(a.id, ScanState::Present).unwrap();
            assert_eq!(repos.list_missing(root.id).unwrap().len(), 1);
        });
    }

    #[test]
    fn find_by_path_variants() {
        with_repos("findpath", |repos| {
            let root = repos.add_root(Path::new("/data/p")).unwrap();
            repos
                .upsert_file(FileUpsert {
                    root_id: root.id,
                    rel_path: PathBuf::from("x.png"),
                    display_name: "x.png".to_string(),
                    byte_size: 1,
                    mtime_ms: 1,
                    scan_state: ScanState::Present,
                })
                .unwrap();
            assert!(repos
                .find_by_path(root.id, &PathBuf::from("x.png"))
                .unwrap()
                .is_some());
            assert!(repos
                .find_by_path(root.id, &PathBuf::from("y.png"))
                .unwrap()
                .is_none());
            assert!(repos
                .find_by_path(9999, &PathBuf::from("x.png"))
                .unwrap()
                .is_none());
        });
    }

    #[test]
    fn extension_extraction_dotfiles_and_upper() {
        with_repos("ext", |repos| {
            let root = repos.add_root(Path::new("/data/p")).unwrap();
            let mk = |name: &str| FileUpsert {
                root_id: root.id,
                rel_path: PathBuf::from(name),
                display_name: name.to_string(),
                byte_size: 1,
                mtime_ms: 1,
                scan_state: ScanState::Present,
            };
            let png = repos.upsert_file(mk("IMG.PNG")).unwrap();
            assert_eq!(png.extension.as_deref(), Some("png"), "小文字化");
            let dot = repos.upsert_file(mk(".hidden")).unwrap();
            assert_eq!(dot.extension, None, "dotfileは拡張子なし");
            let none = repos.upsert_file(mk("README")).unwrap();
            assert_eq!(none.extension, None);
        });
    }

    // ===== AssetRepo =====

    fn new_asset(hash: &str, size: u64) -> NewAsset {
        NewAsset {
            content_hash: format!("blake3:{hash}"),
            hash_algorithm: "blake3".to_string(),
            byte_size: size,
            mime_type: Some("image/png".to_string()),
        }
    }

    #[test]
    fn create_find_and_duplicate_asset() {
        with_repos("asset", |repos| {
            let a = repos.create_asset(new_asset("aa", 100)).unwrap();
            assert_eq!(a.content_hash, "blake3:aa");
            assert_eq!(a.hash_state, HashState::Done);
            assert_eq!(a.rating, 0);
            assert_eq!(a.triage, TriageState::Unselected);
            assert!(!a.favorite);
            assert_eq!(a.mime_type.as_deref(), Some("image/png"));

            let found = repos.find_by_hash("blake3:aa").unwrap().unwrap();
            assert_eq!(found.id, a.id);
            assert!(repos.find_by_hash("blake3:zz").unwrap().is_none());

            let err = repos.create_asset(new_asset("aa", 100)).unwrap_err();
            assert_eq!(err, RepoError::Duplicate, "content_hash一意");
        });
    }

    #[test]
    fn link_file_to_asset_flow() {
        with_repos("link", |repos| {
            let root = repos.add_root(Path::new("/data/p")).unwrap();
            // 走査: まずFileだけ (Asset前)
            let f = repos
                .upsert_file(FileUpsert {
                    root_id: root.id,
                    rel_path: PathBuf::from("i.png"),
                    display_name: "i.png".to_string(),
                    byte_size: 42,
                    mtime_ms: 1,
                    scan_state: ScanState::Present,
                })
                .unwrap();
            assert!(f.asset_id.is_none());

            let asset = repos.create_asset(new_asset("bb", 42)).unwrap();
            repos.link_file(f.id, asset.id).unwrap();

            let linked = repos
                .find_by_path(root.id, &PathBuf::from("i.png"))
                .unwrap()
                .unwrap();
            assert_eq!(linked.asset_id, Some(asset.id));

            // 同一内容の別ファイル → 同一Assetへ (統合の基礎)
            let f2 = repos
                .upsert_file(FileUpsert {
                    root_id: root.id,
                    rel_path: PathBuf::from("copy/i.png"),
                    display_name: "i.png".to_string(),
                    byte_size: 42,
                    mtime_ms: 1,
                    scan_state: ScanState::Present,
                })
                .unwrap();
            let existing = repos.find_by_hash("blake3:bb").unwrap().unwrap();
            repos.link_file(f2.id, existing.id).unwrap();

            // NotFound経路
            assert_eq!(
                repos.link_file(9999, asset.id).unwrap_err(),
                RepoError::NotFound
            );
            assert_eq!(
                repos.link_file(f.id, 9999).unwrap_err(),
                RepoError::NotFound
            );
        });
    }

    #[test]
    fn asset_delete_is_restricted_while_file_links() {
        with_repos("restrict", |repos| {
            let root = repos.add_root(Path::new("/data/p")).unwrap();
            let f = repos
                .upsert_file(FileUpsert {
                    root_id: root.id,
                    rel_path: PathBuf::from("i.png"),
                    display_name: "i.png".to_string(),
                    byte_size: 1,
                    mtime_ms: 1,
                    scan_state: ScanState::Present,
                })
                .unwrap();
            let asset = repos.create_asset(new_asset("cc", 1)).unwrap();
            repos.link_file(f.id, asset.id).unwrap();

            // AssetはRESTRICT: リンク中の削除はFK違反で拒否
            let err = repos
                .conn()
                .execute("DELETE FROM assets WHERE id = ?1", [asset.id])
                .unwrap_err();
            match map_err(err) {
                RepoError::Db(msg) => assert!(msg.contains("foreign key"), "{msg}"),
                other => panic!("expected FK violation, got {other:?}"),
            }
            // root削除 (files CASCADE) 後はAsset削除可能
            repos.remove_root(root.id).unwrap();
            assert_eq!(
                repos
                    .conn()
                    .execute("DELETE FROM assets WHERE id = ?1", [asset.id])
                    .unwrap(),
                1
            );
        });
    }

    #[test]
    fn raw_fk_violation_insert_is_error() {
        with_repos("rawfk", |repos| {
            // FK有効時に存在しないrootへfiles挿入 → Err (受入条件3)
            let err = repos
                .conn()
                .execute(
                    "INSERT INTO files (uuid, root_id, rel_path, path_encoding, display_name,
                        byte_size, mtime_ms, scan_state, last_seen_at, created_at, updated_at)
                     VALUES ('00000000-0000-7000-8000-000000000009', 12345, x'78',
                        'utf8', 'x', 1, 1, 'present', '2026-08-30T00:00:00.000Z',
                        '2026-08-30T00:00:00.000Z', '2026-08-30T00:00:00.000Z')",
                    [],
                )
                .unwrap_err();
            assert!(
                err.to_string().to_lowercase().contains("foreign key"),
                "{err}"
            );
        });
    }

    // ===== 非UTF-8パス (受入条件・Unix) =====

    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_round_trip_through_db() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        with_repos("nonutf8", |repos| {
            // 非UTF-8のルートパス + 非UTF-8の相対パス
            let root_path = PathBuf::from(OsString::from_vec(b"/data/\xff\xfe photos".to_vec()));
            let root = repos.add_root(&root_path).unwrap();
            assert_eq!(root.path, root_path, "ルートパス完全復元");
            assert!(root.display_path.contains("\u{fffd}"), "表示名はlossy");

            let rel = PathBuf::from(OsString::from_vec(b"\x80dir/\xff.png".to_vec()));
            let f = repos
                .upsert_file(FileUpsert {
                    root_id: root.id,
                    rel_path: rel.clone(),
                    display_name: String::from_utf8_lossy(b"\xff.png").into_owned(),
                    byte_size: 7,
                    mtime_ms: 9,
                    scan_state: ScanState::Present,
                })
                .unwrap();
            let back = repos.find_by_path(root.id, &rel).unwrap().unwrap();
            assert_eq!(back.id, f.id);
            assert_eq!(back.rel_path, rel, "相対パス完全復元 (BLOB往復)");
            assert_eq!(back.display_name, String::from_utf8_lossy(b"\xff.png"));

            // 再upsert (同一バイト列) が同一行に当たることも確認
            let again = repos
                .upsert_file(FileUpsert {
                    root_id: root.id,
                    rel_path: rel,
                    display_name: String::from_utf8_lossy(b"\xff.png").into_owned(),
                    byte_size: 8,
                    mtime_ms: 10,
                    scan_state: ScanState::Present,
                })
                .unwrap();
            assert_eq!(again.id, f.id);
            assert_eq!(again.byte_size, 8);
        });
    }

    #[test]
    fn oversized_u64_inputs_are_errors_not_wrapped() {
        // u64→i64の直接castはオーバーフローで負値にラップするため禁止。
        // i64範囲超えは明示的なErrとする。
        with_repos("overflow", |repos| {
            let root = repos.add_root(Path::new("/data/p")).unwrap();
            let mk = |size: u64, mtime: u64| FileUpsert {
                root_id: root.id,
                rel_path: PathBuf::from("big.png"),
                display_name: "big.png".to_string(),
                byte_size: size,
                mtime_ms: mtime,
                scan_state: ScanState::Present,
            };
            let err = repos.upsert_file(mk(u64::MAX, 1)).unwrap_err();
            assert!(err.to_string().contains("byte_size"), "{err}");
            let err = repos.upsert_file(mk(1, u64::MAX)).unwrap_err();
            assert!(err.to_string().contains("mtime_ms"), "{err}");
            let err = repos.create_asset(new_asset("dd", u64::MAX)).unwrap_err();
            assert!(err.to_string().contains("byte_size"), "{err}");
        });
    }

    #[test]
    fn set_scan_state_does_not_update_last_seen_at() {
        // last_seen_at = 「最後にファイルの存在を確認した時刻」。missingへの
        // 状態変更は存在確認ではないため更新しない。存在確認を反映するのは
        // upsert (発見) のみ。
        with_repos("lastseen", |repos| {
            let root = repos.add_root(Path::new("/data/p")).unwrap();
            let mk = || FileUpsert {
                root_id: root.id,
                rel_path: PathBuf::from("a.png"),
                display_name: "a.png".to_string(),
                byte_size: 1,
                mtime_ms: 1,
                scan_state: ScanState::Present,
            };
            let f = repos.upsert_file(mk()).unwrap();
            let sentinel = "2000-01-01T00:00:00.000Z";
            repos
                .conn()
                .execute(
                    "UPDATE files SET last_seen_at = ?1 WHERE id = ?2",
                    params![sentinel, f.id],
                )
                .unwrap();

            repos.set_scan_state(f.id, ScanState::Missing).unwrap();
            let back = repos
                .find_by_path(root.id, &PathBuf::from("a.png"))
                .unwrap()
                .unwrap();
            assert_eq!(back.scan_state, ScanState::Missing);
            assert_eq!(
                back.last_seen_at, sentinel,
                "set_scan_stateはlast_seen_atを更新しない"
            );
            assert_ne!(back.updated_at, sentinel, "updated_atは更新される");

            // 再upsert = 存在確認 → last_seen_atは更新される
            repos.upsert_file(mk()).unwrap();
            let back = repos
                .find_by_path(root.id, &PathBuf::from("a.png"))
                .unwrap()
                .unwrap();
            assert_ne!(back.last_seen_at, sentinel);
        });
    }

    #[test]
    fn not_null_and_check_violations_are_constraint_not_duplicate() {
        // CHECK/NOT NULL違反をDuplicateと誤分類すると、呼び出し側が
        // 「重複」として処理してしまうため区別する。
        with_repos("constraint", |repos| {
            let root = repos.add_root(Path::new("/data/p")).unwrap();
            // NOT NULL違反 (display_name NULL)
            let err = repos
                .conn()
                .execute(
                    "INSERT INTO files (uuid, root_id, rel_path, path_encoding, display_name,
                         byte_size, mtime_ms, scan_state, last_seen_at, created_at, updated_at)
                     VALUES ('00000000-0000-7000-8000-00000000000a', ?1, x'61', 'utf8', NULL,
                         1, 1, 'present', '2026-08-30T00:00:00.000Z',
                         '2026-08-30T00:00:00.000Z', '2026-08-30T00:00:00.000Z')",
                    [root.id],
                )
                .unwrap_err();
            match map_err(err) {
                RepoError::Constraint(msg) => {
                    assert!(msg.contains("NOT NULL"), "{msg}");
                }
                other => panic!("expected Constraint, got {other:?}"),
            }

            // CHECK違反 (rating 0..5)
            let err = repos
                .conn()
                .execute(
                    "INSERT INTO assets (uuid, content_hash, hash_algorithm, byte_size,
                         rating, created_at, updated_at)
                     VALUES ('00000000-0000-7000-8000-00000000000b', 'blake3:ee', 'blake3', 1,
                         6, '2026-08-30T00:00:00.000Z', '2026-08-30T00:00:00.000Z')",
                    [],
                )
                .unwrap_err();
            match map_err(err) {
                RepoError::Constraint(msg) => {
                    assert!(msg.to_lowercase().contains("check"), "{msg}");
                }
                other => panic!("expected Constraint, got {other:?}"),
            }

            // 一意制約は引き続きDuplicate
            repos.add_root(Path::new("/data/dup")).unwrap();
            let err = repos.add_root(Path::new("/data/dup")).unwrap_err();
            assert_eq!(err, RepoError::Duplicate);
        });
    }

    #[test]
    fn path_is_within_helper_used_by_overlap() {
        // coreのhelperとDB実装の整合 (同一ロジックを使用している証拠の最小確認)
        assert!(path_is_within(Path::new("/a/b"), Path::new("/a")));
    }
}
