//! repository traitのSQLite実装 (MVP WP-1)。
//!
//! [`SqliteRepos`] は1つの[`rusqlite::Connection`]の上に
//! [`gento_core::repo`]の3 trait (LibraryRootRepo / FileRepo / AssetRepo)
//! を実装する。トランザクション境界は呼び出し側
//! ([`crate::Db::with_writer`] 等)が持つ — 各メソッドは単文atomic。

use std::path::Path;

use gento_core::repo::{
    AssetRecord, AssetRepo, FileRecord, FileRepo, FileUpsert, HashState, LibraryRootRecord,
    LibraryRootRepo, NewAsset, OsFileId, RepoError, RootSettings, ScanState, TriageState,
};
use rusqlite::{params, Connection, OptionalExtension};

use crate::ids::{new_uuid_v7, now_utc_rfc3339};
use crate::paths::{decode_path, encode_path, PathEncoding};

/// ルート行のSELECT列一覧 (map_library_rootと共通)。
const ROOT_COLUMNS: &str = "id, uuid, path, path_encoding, display_path, enabled, \
     watch_enabled, ai_enabled, send_exif, send_gps, created_at, updated_at";

/// files行のSELECT列一覧 (map_fileと共通)。
const FILE_COLUMNS: &str = "id, uuid, root_id, asset_id, rel_path, path_encoding, display_name, \
     extension, byte_size, mtime_ms, mtime_ns, os_file_id, scan_state, last_seen_at, created_at, \
     updated_at";

/// assets行のSELECT列一覧 (map_assetと共通)。
const ASSET_COLUMNS: &str = "id, uuid, content_hash, hash_algorithm, hash_state, mime_type, \
     byte_size, width, height, has_alpha, is_animated, frame_count, rating, triage, favorite, \
     created_at, updated_at";

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
        let sql = format!("SELECT {ROOT_COLUMNS} FROM library_roots WHERE id = ?1");
        self.conn
            .query_row(&sql, [id], map_library_root)
            .optional()
            .map_err(map_err)
    }
}

/// rusqliteエラー → [`RepoError`]。
/// - 一意制約違反 (UNIQUE/PK) → [`RepoError::Duplicate`]
/// - CHECK・NOT NULL等、一意・FK以外の制約違反 → [`RepoError::Constraint`]
///   (Duplicateと誤分類すると呼び出し側が「重複」処理をしてしまうため区別)
/// - 外部キー違反 → 「foreign key violation」付きの [`RepoError::Db`]
pub(crate) fn map_err(e: rusqlite::Error) -> RepoError {
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
        watch_enabled: row.get::<_, i64>("watch_enabled")? != 0,
        ai_enabled: row.get::<_, i64>("ai_enabled")? != 0,
        send_exif: row.get::<_, i64>("send_exif")? != 0,
        send_gps: row.get::<_, i64>("send_gps")? != 0,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

fn map_file(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileRecord> {
    let rel_bytes: Vec<u8> = row.get("rel_path")?;
    let scan_state_str: String = row.get("scan_state")?;
    let scan_state = scan_state_from_db_value(&scan_state_str)?;
    let os_file_id_bytes: Option<Vec<u8>> = row.get("os_file_id")?;
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
        mtime_ns: row
            .get::<_, Option<i64>>("mtime_ns")?
            .and_then(|v| u64::try_from(v).ok()),
        os_file_id: os_file_id_bytes.as_deref().and_then(OsFileId::from_bytes),
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
        width: row
            .get::<_, Option<i64>>("width")?
            .and_then(|v| u32::try_from(v).ok()),
        height: row
            .get::<_, Option<i64>>("height")?
            .and_then(|v| u32::try_from(v).ok()),
        has_alpha: row.get::<_, Option<i64>>("has_alpha")?.map(|v| v != 0),
        is_animated: row.get::<_, Option<i64>>("is_animated")?.map(|v| v != 0),
        frame_count: row
            .get::<_, Option<i64>>("frame_count")?
            .and_then(|v| u32::try_from(v).ok()),
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
        let sql = format!("SELECT {ROOT_COLUMNS} FROM library_roots ORDER BY id");
        let mut stmt = self.conn.prepare(&sql).map_err(map_err)?;
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

    fn set_root_settings(&self, root_id: i64, settings: RootSettings) -> Result<(), RepoError> {
        let changed = self
            .conn
            .execute(
                "UPDATE library_roots SET
                    watch_enabled = ?2, ai_enabled = ?3, send_exif = ?4, send_gps = ?5,
                    updated_at = ?6
                 WHERE id = ?1",
                params![
                    root_id,
                    settings.watch_enabled as i64,
                    settings.ai_enabled as i64,
                    settings.send_exif as i64,
                    settings.send_gps as i64,
                    now_utc_rfc3339(),
                ],
            )
            .map_err(map_err)?;
        if changed == 0 {
            return Err(RepoError::NotFound);
        }
        Ok(())
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
                     extension, byte_size, mtime_ms, mtime_ns, os_file_id, scan_state,
                     last_seen_at, created_at, updated_at)
                  VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12, ?12)
                  ON CONFLICT (root_id, path_encoding, rel_path) DO UPDATE SET
                     display_name = excluded.display_name,
                     extension = excluded.extension,
                     byte_size = excluded.byte_size,
                     mtime_ms = excluded.mtime_ms,
                     mtime_ns = excluded.mtime_ns,
                     os_file_id = excluded.os_file_id,
                     scan_state = excluded.scan_state,
                     last_seen_at = excluded.last_seen_at,
                     -- 内容変化検出時のみassetリンク解除 (NULLへ)。それ以外は保持。
                     asset_id = CASE WHEN ?13 THEN NULL ELSE files.asset_id END,
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
                    u64_to_i64(file.mtime_ns, "mtime_ns")?,
                    file.os_file_id.map(|id| id.to_bytes().to_vec()),
                    file.scan_state.as_str(),
                    now,
                    file.detach_asset as i64,
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
        let sql = format!(
            "SELECT {FILE_COLUMNS} FROM files
             WHERE root_id = ?1 AND scan_state = 'missing' ORDER BY id"
        );
        let mut stmt = self.conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map([root_id], map_file)
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    fn find_by_path(&self, root_id: i64, rel_path: &Path) -> Result<Option<FileRecord>, RepoError> {
        let (rel_blob, enc) = encode_path(rel_path);
        let sql = format!(
            "SELECT {FILE_COLUMNS} FROM files
             WHERE root_id = ?1 AND path_encoding = ?2 AND rel_path = ?3"
        );
        self.conn
            .query_row(&sql, params![root_id, enc.as_str(), rel_blob], map_file)
            .optional()
            .map_err(map_err)
    }

    fn get_file(&self, file_id: i64) -> Result<Option<FileRecord>, RepoError> {
        let sql = format!("SELECT {FILE_COLUMNS} FROM files WHERE id = ?1");
        self.conn
            .query_row(&sql, [file_id], map_file)
            .optional()
            .map_err(map_err)
    }

    fn upsert_files_batch(&self, files: &[FileUpsert]) -> Result<Vec<FileRecord>, RepoError> {
        if files.is_empty() {
            return Ok(Vec::new());
        }
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| RepoError::Db(e.to_string()))?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO files
                        (uuid, root_id, asset_id, rel_path, path_encoding, display_name,
                         extension, byte_size, mtime_ms, mtime_ns, os_file_id, scan_state,
                         last_seen_at, created_at, updated_at)
                      VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12, ?12)
                      ON CONFLICT (root_id, path_encoding, rel_path) DO UPDATE SET
                        display_name = excluded.display_name,
                        extension = excluded.extension,
                        byte_size = excluded.byte_size,
                        mtime_ms = excluded.mtime_ms,
                        mtime_ns = excluded.mtime_ns,
                        os_file_id = excluded.os_file_id,
                        scan_state = excluded.scan_state,
                        last_seen_at = excluded.last_seen_at,
                        asset_id = CASE WHEN ?13 THEN NULL ELSE files.asset_id END,
                        updated_at = excluded.updated_at",
                )
                .map_err(map_err)?;
            for file in files {
                let (rel_blob, enc) = encode_path(&file.rel_path);
                let now = now_utc_rfc3339();
                stmt.execute(params![
                    new_uuid_v7(),
                    file.root_id,
                    rel_blob,
                    enc.as_str(),
                    file.display_name,
                    extension_of(&file.display_name),
                    u64_to_i64(file.byte_size, "byte_size")?,
                    u64_to_i64(file.mtime_ms, "mtime_ms")?,
                    u64_to_i64(file.mtime_ns, "mtime_ns")?,
                    file.os_file_id.map(|id| id.to_bytes().to_vec()),
                    file.scan_state.as_str(),
                    now,
                    file.detach_asset as i64,
                ])
                .map_err(map_err)?;
            }
        }
        tx.commit().map_err(map_err)?;

        // コミット済み行を引き直して返す (batch keysの順で)。
        files
            .iter()
            .map(|file| {
                self.find_by_path(file.root_id, &file.rel_path)?
                    .ok_or_else(|| RepoError::Db("upserted file row is not readable".to_string()))
            })
            .collect()
    }

    fn find_by_os_file_id(
        &self,
        root_id: i64,
        os_file_id: &OsFileId,
    ) -> Result<Option<FileRecord>, RepoError> {
        let sql = format!(
            "SELECT {FILE_COLUMNS} FROM files
             WHERE root_id = ?1 AND os_file_id = ?2 ORDER BY id LIMIT 1"
        );
        self.conn
            .query_row(
                &sql,
                params![root_id, os_file_id.to_bytes().to_vec()],
                map_file,
            )
            .optional()
            .map_err(map_err)
    }

    fn relocate_file(&self, file_id: i64, file: FileUpsert) -> Result<FileRecord, RepoError> {
        let (rel_blob, enc) = encode_path(&file.rel_path);
        let now = now_utc_rfc3339();
        let changed = self
            .conn
            .execute(
                "UPDATE files SET
                    rel_path = ?2, path_encoding = ?3, display_name = ?4,
                    extension = ?5, byte_size = ?6, mtime_ms = ?7, mtime_ns = ?8,
                    os_file_id = ?9, scan_state = ?10, last_seen_at = ?11, updated_at = ?11
                 WHERE id = ?1",
                params![
                    file_id,
                    rel_blob,
                    enc.as_str(),
                    file.display_name,
                    extension_of(&file.display_name),
                    u64_to_i64(file.byte_size, "byte_size")?,
                    u64_to_i64(file.mtime_ms, "mtime_ms")?,
                    u64_to_i64(file.mtime_ns, "mtime_ns")?,
                    file.os_file_id.map(|id| id.to_bytes().to_vec()),
                    file.scan_state.as_str(),
                    now,
                ],
            )
            .map_err(map_err)?;
        if changed == 0 {
            return Err(RepoError::NotFound);
        }
        // rel_path一意制約違反はDuplicateで伝える (呼び出し側はupsertへフォールバック)
        self.conn
            .query_row(
                &format!("SELECT {FILE_COLUMNS} FROM files WHERE id = ?1"),
                [file_id],
                map_file,
            )
            .map_err(map_err)
    }

    fn set_scan_state_batch(&self, file_ids: &[i64], state: ScanState) -> Result<usize, RepoError> {
        if file_ids.is_empty() {
            return Ok(0);
        }
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| RepoError::Db(e.to_string()))?;
        let changed = {
            let mut stmt = tx
                .prepare(
                    "UPDATE files SET scan_state = ?2, updated_at = ?3
                     WHERE id = ?1 AND scan_state != ?2",
                )
                .map_err(map_err)?;
            let mut changed = 0;
            for id in file_ids {
                changed += stmt
                    .execute(params![id, state.as_str(), now_utc_rfc3339()])
                    .map_err(map_err)?;
            }
            changed
        };
        tx.commit().map_err(map_err)?;
        Ok(changed)
    }

    fn list_files_paged(
        &self,
        root_id: i64,
        offset: u32,
        limit: u32,
    ) -> Result<(Vec<FileRecord>, u64), RepoError> {
        // countとpageを1スナップショットで読む (並行書き込みで2文の間に
        // 行が増えると total < page.len() が起こる。差分走査のページング
        // 一貫性に効くため文レベルではなくトランザクションで束ねる)。
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| RepoError::Db(e.to_string()))?;
        let total: i64 = tx
            .query_row(
                "SELECT count(*) FROM files WHERE root_id = ?1",
                [root_id],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        let sql = format!(
            "SELECT {FILE_COLUMNS} FROM files WHERE root_id = ?1 ORDER BY id LIMIT ?2 OFFSET ?3"
        );
        let mut stmt = tx.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(params![root_id, limit, offset], map_file)
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        drop(stmt);
        tx.commit().map_err(map_err)?;
        Ok((rows, total.max(0) as u64))
    }

    fn list_present_joined(&self) -> Result<Vec<(FileRecord, Option<AssetRecord>)>, RepoError> {
        const SQL: &str =
            "SELECT f.id AS f_id, f.uuid AS f_uuid, f.root_id, f.asset_id, f.rel_path,
                    f.path_encoding, f.display_name, f.extension, f.byte_size, f.mtime_ms,
                    f.mtime_ns, f.os_file_id, f.scan_state, f.last_seen_at,
                    f.created_at AS f_created_at, f.updated_at AS f_updated_at,
                    a.id AS a_id, a.uuid AS a_uuid, a.content_hash, a.hash_algorithm,
                    a.hash_state, a.mime_type, a.byte_size AS a_byte_size, a.width, a.height,
                    a.has_alpha, a.is_animated, a.frame_count,
                    a.rating, a.triage, a.favorite, a.created_at AS a_created_at,
                    a.updated_at AS a_updated_at
             FROM files f LEFT JOIN assets a ON f.asset_id = a.id
             WHERE f.scan_state = 'present'
             ORDER BY f.root_id, f.rel_path";
        let mut stmt = self.conn.prepare(SQL).map_err(map_err)?;
        let rows = stmt
            .query_map([], |row| {
                let has_asset: Option<i64> = row.get("a_id")?;
                let file = map_file_prefixed(row)?;
                let asset = if has_asset.is_some() {
                    Some(map_asset_prefixed(row)?)
                } else {
                    None
                };
                Ok((file, asset))
            })
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    fn list_files_without_asset(&self, root_id: i64) -> Result<Vec<FileRecord>, RepoError> {
        let sql = format!(
            "SELECT {FILE_COLUMNS} FROM files
             WHERE root_id = ?1 AND asset_id IS NULL AND scan_state = 'present' ORDER BY id"
        );
        let mut stmt = self.conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map([root_id], map_file)
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    fn delete_file(&self, file_id: i64) -> Result<(), RepoError> {
        let changed = self
            .conn
            .execute("DELETE FROM files WHERE id = ?1", [file_id])
            .map_err(map_err)?;
        if changed == 0 {
            return Err(RepoError::NotFound);
        }
        Ok(())
    }
}

// ===== WP-7: ファイル操作履歴 =====

impl gento_core::file_ops::FileOpsRepo for SqliteRepos<'_> {
    fn insert_operation(
        &self,
        op: gento_core::file_ops::NewFileOp,
    ) -> Result<gento_core::file_ops::FileOpRecord, RepoError> {
        self.conn
            .execute(
                "INSERT INTO file_operations
                     (uuid, operation_type, status, undoable, request_json, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    new_uuid_v7(),
                    op.operation_type.as_str(),
                    op.status.as_str(),
                    op.undoable as i64,
                    op.request_json,
                    now_utc_rfc3339(),
                ],
            )
            .map_err(map_err)?;
        let id = self.conn.last_insert_rowid();
        self.get_operation(id)?
            .ok_or_else(|| RepoError::Db("inserted file_operation row is not readable".to_string()))
    }

    fn update_operation(
        &self,
        id: i64,
        status: gento_core::file_ops::FileOpStatus,
        result_json: Option<&str>,
        undoable: bool,
    ) -> Result<(), RepoError> {
        let changed = self
            .conn
            .execute(
                "UPDATE file_operations
                 SET status = ?2, result_json = ?3, undoable = ?4
                 WHERE id = ?1",
                params![id, status.as_str(), result_json, undoable as i64],
            )
            .map_err(map_err)?;
        if changed == 0 {
            return Err(RepoError::NotFound);
        }
        Ok(())
    }

    fn mark_undone(&self, id: i64) -> Result<(), RepoError> {
        let changed = self
            .conn
            .execute(
                "UPDATE file_operations
                 SET status = 'undone', undone_at = ?2, undoable = 0
                 WHERE id = ?1",
                params![id, now_utc_rfc3339()],
            )
            .map_err(map_err)?;
        if changed == 0 {
            return Err(RepoError::NotFound);
        }
        Ok(())
    }

    fn get_operation(
        &self,
        id: i64,
    ) -> Result<Option<gento_core::file_ops::FileOpRecord>, RepoError> {
        self.conn
            .query_row(
                &format!("SELECT {FILE_OP_COLUMNS} FROM file_operations WHERE id = ?1"),
                [id],
                map_file_operation,
            )
            .optional()
            .map_err(map_err)
    }

    fn list_recent_operations(
        &self,
        limit: u32,
    ) -> Result<Vec<gento_core::file_ops::FileOpRecord>, RepoError> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT {FILE_OP_COLUMNS} FROM file_operations ORDER BY id DESC LIMIT ?1"
            ))
            .map_err(map_err)?;
        let rows = stmt
            .query_map([limit as i64], map_file_operation)
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    fn list_executing_operations(
        &self,
    ) -> Result<Vec<gento_core::file_ops::FileOpRecord>, RepoError> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT {FILE_OP_COLUMNS} FROM file_operations
                 WHERE status = 'executing' ORDER BY id"
            ))
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], map_file_operation)
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }
}

/// file_operations行のSELECT列一覧 (map_file_operationと共通)。
const FILE_OP_COLUMNS: &str = "id, uuid, operation_type, status, undoable, request_json, \
     result_json, created_at, undone_at";

fn map_file_operation(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<gento_core::file_ops::FileOpRecord> {
    use gento_core::file_ops::{FileOpKind, FileOpStatus};
    use std::str::FromStr;
    let kind_str: String = row.get("operation_type")?;
    let status_str: String = row.get("status")?;
    let parse = |s: &str| -> rusqlite::Result<_> {
        FileOpKind::from_str(s).map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                format!("unknown operation_type: {s}").into(),
            )
        })
    };
    let parse_status = |s: &str| -> rusqlite::Result<_> {
        FileOpStatus::from_str(s).map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                format!("unknown file_operations.status: {s}").into(),
            )
        })
    };
    Ok(gento_core::file_ops::FileOpRecord {
        id: row.get("id")?,
        uuid: row.get("uuid")?,
        operation_type: parse(&kind_str)?,
        status: parse_status(&status_str)?,
        undoable: row.get::<_, i64>("undoable")? != 0,
        request_json: row.get("request_json")?,
        result_json: row.get("result_json")?,
        created_at: row.get("created_at")?,
        undone_at: row.get("undone_at")?,
    })
}

/// JOINクエリ用: files列をエイリアス付きで読む (map_fileと同意味)。
pub(crate) fn map_file_prefixed(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileRecord> {
    let rel_bytes: Vec<u8> = row.get("rel_path")?;
    let scan_state_str: String = row.get("scan_state")?;
    let scan_state = scan_state_from_db_value(&scan_state_str)?;
    let os_file_id_bytes: Option<Vec<u8>> = row.get("os_file_id")?;
    Ok(FileRecord {
        id: row.get("f_id")?,
        uuid: row.get("f_uuid")?,
        root_id: row.get("root_id")?,
        asset_id: row.get("asset_id")?,
        rel_path: decode_path(&rel_bytes),
        display_name: row.get("display_name")?,
        extension: row.get("extension")?,
        byte_size: row.get::<_, i64>("byte_size")?.max(0) as u64,
        mtime_ms: row.get("mtime_ms")?,
        mtime_ns: row
            .get::<_, Option<i64>>("mtime_ns")?
            .and_then(|v| u64::try_from(v).ok()),
        os_file_id: os_file_id_bytes.as_deref().and_then(OsFileId::from_bytes),
        scan_state,
        last_seen_at: row.get("last_seen_at")?,
        created_at: row.get("f_created_at")?,
        updated_at: row.get("f_updated_at")?,
    })
}

/// JOINクエリ用: assets列をエイリアス付きで読む (map_assetと同意味)。
pub(crate) fn map_asset_prefixed(row: &rusqlite::Row<'_>) -> rusqlite::Result<AssetRecord> {
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
        id: row.get("a_id")?,
        uuid: row.get("a_uuid")?,
        content_hash: row.get("content_hash")?,
        hash_algorithm: row.get("hash_algorithm")?,
        hash_state,
        byte_size: row.get::<_, i64>("a_byte_size")?.max(0) as u64,
        mime_type: row.get("mime_type")?,
        width: row
            .get::<_, Option<i64>>("width")?
            .and_then(|v| u32::try_from(v).ok()),
        height: row
            .get::<_, Option<i64>>("height")?
            .and_then(|v| u32::try_from(v).ok()),
        has_alpha: row.get::<_, Option<i64>>("has_alpha")?.map(|v| v != 0),
        is_animated: row.get::<_, Option<i64>>("is_animated")?.map(|v| v != 0),
        frame_count: row
            .get::<_, Option<i64>>("frame_count")?
            .and_then(|v| u32::try_from(v).ok()),
        rating: row.get::<_, i64>("rating")?.clamp(0, 5) as u8,
        triage,
        favorite: row.get::<_, i64>("favorite")? != 0,
        created_at: row.get("a_created_at")?,
        updated_at: row.get("a_updated_at")?,
    })
}

impl AssetRepo for SqliteRepos<'_> {
    fn create_asset(&self, asset: NewAsset) -> Result<AssetRecord, RepoError> {
        let now = now_utc_rfc3339();
        self.conn
            .execute(
                "INSERT INTO assets
                    (uuid, content_hash, hash_algorithm, hash_state, mime_type,
                     byte_size, width, height, has_alpha, is_animated, frame_count,
                     rating, triage, favorite, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 'done', ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, 'unselected', 0, ?11, ?11)",
                params![
                    new_uuid_v7(),
                    asset.content_hash,
                    asset.hash_algorithm,
                    asset.mime_type,
                    u64_to_i64(asset.byte_size, "byte_size")?,
                    u32_opt_to_i64(asset.width, "width")?,
                    u32_opt_to_i64(asset.height, "height")?,
                    asset.has_alpha.map(|b| b as i64),
                    asset.is_animated.map(|b| b as i64),
                    u32_opt_to_i64(asset.frame_count, "frame_count")?,
                    now,
                ],
            )
            .map_err(map_err)?;
        let id = self.conn.last_insert_rowid();
        let sql = format!("SELECT {ASSET_COLUMNS} FROM assets WHERE id = ?1");
        self.conn.query_row(&sql, [id], map_asset).map_err(map_err)
    }

    fn find_by_hash(&self, content_hash: &str) -> Result<Option<AssetRecord>, RepoError> {
        let sql = format!("SELECT {ASSET_COLUMNS} FROM assets WHERE content_hash = ?1");
        self.conn
            .query_row(&sql, [content_hash], map_asset)
            .optional()
            .map_err(map_err)
    }

    fn get_asset(&self, asset_id: i64) -> Result<Option<AssetRecord>, RepoError> {
        let sql = format!("SELECT {ASSET_COLUMNS} FROM assets WHERE id = ?1");
        self.conn
            .query_row(&sql, [asset_id], map_asset)
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

    fn create_or_get_asset(&self, asset: NewAsset) -> Result<(AssetRecord, bool), RepoError> {
        if let Some(existing) = self.find_by_hash(&asset.content_hash)? {
            return Ok((existing, false));
        }
        let hash = asset.content_hash.clone();
        match self.create_asset(asset) {
            Ok(record) => Ok((record, true)),
            // 単一writer内では発生し得ないが、将来の並行経路に備えた再確認。
            Err(RepoError::Duplicate) => {
                let existing = self.find_by_hash(&hash)?.ok_or(RepoError::Duplicate)?;
                Ok((existing, false))
            }
            Err(err) => Err(err),
        }
    }

    fn set_rating(&self, asset_id: i64, rating: u8) -> Result<(), RepoError> {
        if rating > 5 {
            return Err(RepoError::Constraint("rating must be 0..=5".to_string()));
        }
        let changed = self
            .conn
            .execute(
                "UPDATE assets SET rating = ?2, updated_at = ?3 WHERE id = ?1",
                params![asset_id, rating, now_utc_rfc3339()],
            )
            .map_err(map_err)?;
        if changed == 0 {
            return Err(RepoError::NotFound);
        }
        Ok(())
    }

    fn set_cull_state(&self, asset_id: i64, triage: TriageState) -> Result<(), RepoError> {
        let changed = self
            .conn
            .execute(
                "UPDATE assets SET triage = ?2, updated_at = ?3 WHERE id = ?1",
                params![asset_id, triage.as_str(), now_utc_rfc3339()],
            )
            .map_err(map_err)?;
        if changed == 0 {
            return Err(RepoError::NotFound);
        }
        Ok(())
    }

    fn set_favorite(&self, asset_id: i64, favorite: bool) -> Result<(), RepoError> {
        let changed = self
            .conn
            .execute(
                "UPDATE assets SET favorite = ?2, updated_at = ?3 WHERE id = ?1",
                params![asset_id, favorite as i64, now_utc_rfc3339()],
            )
            .map_err(map_err)?;
        if changed == 0 {
            return Err(RepoError::NotFound);
        }
        Ok(())
    }

    fn set_rating_batch(&self, asset_ids: &[i64], rating: u8) -> Result<(), RepoError> {
        self.update_assets_batch(
            asset_ids,
            "rating",
            rusqlite::types::Value::Integer(rating as i64),
        )
    }

    fn set_cull_state_batch(
        &self,
        asset_ids: &[i64],
        triage: TriageState,
    ) -> Result<(), RepoError> {
        self.update_assets_batch(
            asset_ids,
            "triage",
            rusqlite::types::Value::Text(triage.as_str().to_string()),
        )
    }

    fn set_favorite_batch(&self, asset_ids: &[i64], favorite: bool) -> Result<(), RepoError> {
        self.update_assets_batch(
            asset_ids,
            "favorite",
            rusqlite::types::Value::Integer(favorite as i64),
        )
    }
}

impl<'a> SqliteRepos<'a> {
    /// assets列の一括更新 (WP-5)。1呼び出し=1トランザクション:
    /// 途中の失敗 (不在ID含む) はロールバックされ、一部だけ適用された
    /// 状態を残さない (メタデータ一括編集の整合)。
    ///
    /// `set_column` はこのモジュール内の静的文字列リテラルのみ
    /// (ユーザー入力の挿入はしない)。
    fn update_assets_batch(
        &self,
        asset_ids: &[i64],
        set_column: &'static str,
        value: rusqlite::types::Value,
    ) -> Result<(), RepoError> {
        if asset_ids.is_empty() {
            return Ok(());
        }
        let sql = format!("UPDATE assets SET {set_column} = ?2, updated_at = ?3 WHERE id = ?1");
        let now = now_utc_rfc3339();
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| RepoError::Db(e.to_string()))?;
        {
            let mut stmt = tx.prepare(&sql).map_err(map_err)?;
            for id in asset_ids {
                let changed = stmt.execute(params![id, &value, &now]).map_err(map_err)?;
                if changed == 0 {
                    // 不在ID: バッチ全体をロールバック (部分適用を残さない)
                    return Err(RepoError::NotFound);
                }
            }
        }
        tx.commit().map_err(map_err)?;
        Ok(())
    }
}

/// Option<u32>値をINTEGER列へ格納する (直接castのオーバーフロー禁止)。
fn u32_opt_to_i64(value: Option<u32>, field: &'static str) -> Result<Option<i64>, RepoError> {
    value
        .map(i64::try_from)
        .transpose()
        .map_err(|_| RepoError::Db(format!("{field} exceeds i64 range")))
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
                    mtime_ns: 1_000,
                    scan_state: ScanState::Present,
                    os_file_id: None,
                    detach_asset: false,
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
                    mtime_ns: 1_700_000_000_000_000,
                    scan_state: ScanState::Present,
                    os_file_id: None,
                    detach_asset: false,
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
                    mtime_ns: 1_700_000_999_000_000,
                    scan_state: ScanState::Missing,
                    os_file_id: None,
                    detach_asset: false,
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
                    mtime_ns: 1_000,
                    scan_state: ScanState::Present,
                    os_file_id: None,
                    detach_asset: false,
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
                    mtime_ns: 1_000,
                    scan_state: ScanState::Present,
                    os_file_id: None,
                    detach_asset: false,
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
                    mtime_ns: 1_000,
                    scan_state: ScanState::Present,
                    os_file_id: None,
                    detach_asset: false,
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
                mtime_ns: 1_000,
                scan_state: state,
                os_file_id: None,
                detach_asset: false,
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
                    mtime_ns: 1_000,
                    scan_state: ScanState::Present,
                    os_file_id: None,
                    detach_asset: false,
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

            // 主キー照合 (ハッシュリンク時の世代検証用)
            let found = repos.get_file(1).unwrap().expect("id=1の行");
            assert_eq!(found.rel_path, PathBuf::from("x.png"));
            assert!(repos.get_file(9999).unwrap().is_none());
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
                mtime_ns: 1_000,
                scan_state: ScanState::Present,
                os_file_id: None,
                detach_asset: false,
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
            width: None,
            height: None,
            has_alpha: None,
            is_animated: None,
            frame_count: None,
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
            // WP-3: プローブ由来列のNULL往復
            assert_eq!(found.has_alpha, None);
            assert_eq!(found.is_animated, None);
            assert_eq!(found.frame_count, None);

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
                    mtime_ns: 1_000,
                    scan_state: ScanState::Present,
                    os_file_id: None,
                    detach_asset: false,
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
                    mtime_ns: 1_000,
                    scan_state: ScanState::Present,
                    os_file_id: None,
                    detach_asset: false,
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
                    mtime_ns: 1_000,
                    scan_state: ScanState::Present,
                    os_file_id: None,
                    detach_asset: false,
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
                    mtime_ns: 9_000,
                    scan_state: ScanState::Present,
                    os_file_id: None,
                    detach_asset: false,
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
                    mtime_ns: 10_000,
                    scan_state: ScanState::Present,
                    os_file_id: None,
                    detach_asset: false,
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
            let mk = |size: u64, mtime: u64, mtime_ns: u64| FileUpsert {
                root_id: root.id,
                rel_path: PathBuf::from("big.png"),
                display_name: "big.png".to_string(),
                byte_size: size,
                mtime_ms: mtime,
                mtime_ns,
                scan_state: ScanState::Present,
                os_file_id: None,
                detach_asset: false,
            };
            let err = repos.upsert_file(mk(u64::MAX, 1, 1_000)).unwrap_err();
            assert!(err.to_string().contains("byte_size"), "{err}");
            let err = repos.upsert_file(mk(1, u64::MAX, 1_000)).unwrap_err();
            assert!(err.to_string().contains("mtime_ms"), "{err}");
            let err = repos.upsert_file(mk(1, 1, u64::MAX)).unwrap_err();
            assert!(err.to_string().contains("mtime_ns"), "{err}");
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
                mtime_ns: 1_000,
                scan_state: ScanState::Present,
                os_file_id: None,
                detach_asset: false,
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

    // ===== WP-2: 設定列・os_file_id・バッチ・移動・ページング・JOIN =====

    fn upsert(root_id: i64, rel: &str, size: u64, mtime: u64) -> FileUpsert {
        FileUpsert {
            root_id,
            rel_path: PathBuf::from(rel),
            display_name: rel.rsplit('/').next().unwrap_or(rel).to_string(),
            byte_size: size,
            mtime_ms: mtime,
            mtime_ns: mtime * 1_000_000,
            scan_state: ScanState::Present,
            os_file_id: None,
            detach_asset: false,
        }
    }

    #[test]
    fn root_settings_round_trip_through_repo() {
        with_repos("settings", |repos| {
            let root = repos.add_root(Path::new("/data/p")).unwrap();
            let defaults = repos.list_roots().unwrap().remove(0);
            assert!(defaults.watch_enabled, "schema既定 watch=1");
            assert!(!defaults.ai_enabled);

            repos
                .set_root_settings(
                    root.id,
                    RootSettings {
                        watch_enabled: false,
                        ai_enabled: true,
                        send_exif: true,
                        send_gps: true,
                    },
                )
                .unwrap();
            let updated = repos.list_roots().unwrap().remove(0);
            assert!(!updated.watch_enabled);
            assert!(updated.ai_enabled);
            assert!(updated.send_exif);
            assert!(updated.send_gps);

            assert_eq!(
                repos
                    .set_root_settings(9999, RootSettings::default())
                    .unwrap_err(),
                RepoError::NotFound
            );
        });
    }

    #[test]
    fn upsert_stores_and_reads_os_file_id() {
        with_repos("osfid", |repos| {
            let root = repos.add_root(Path::new("/data/p")).unwrap();
            let mut file = upsert(root.id, "a.png", 1, 1);
            file.os_file_id = Some(OsFileId::new(9, 77));
            repos.upsert_file(file).unwrap();

            let found = repos
                .find_by_os_file_id(root.id, &OsFileId::new(9, 77))
                .unwrap()
                .expect("os_file_id照合");
            assert_eq!(found.rel_path, PathBuf::from("a.png"));
            assert!(repos
                .find_by_os_file_id(root.id, &OsFileId::new(9, 78))
                .unwrap()
                .is_none());
            // 別ルートは対象外
            let root2 = repos.add_root(Path::new("/data/q")).unwrap();
            assert!(repos
                .find_by_os_file_id(root2.id, &OsFileId::new(9, 77))
                .unwrap()
                .is_none());
        });
    }

    #[test]
    fn batch_upsert_is_transactional_and_detaches_on_demand() {
        with_repos("batch", |repos| {
            let root = repos.add_root(Path::new("/data/p")).unwrap();
            let a = repos.upsert_file(upsert(root.id, "a.png", 1, 1)).unwrap();
            let asset = repos.create_asset(new_asset("ba", 1)).unwrap();
            repos.link_file(a.id, asset.id).unwrap();

            let mut changed = upsert(root.id, "a.png", 99, 2);
            changed.detach_asset = true;
            let records = repos
                .upsert_files_batch(&[
                    changed,
                    upsert(root.id, "b.png", 2, 2),
                    upsert(root.id, "c.png", 3, 3),
                ])
                .unwrap();
            assert_eq!(records.len(), 3);
            assert_eq!(records[0].asset_id, None, "detach指定はリンク解除");
            assert_eq!(records[1].byte_size, 2);
            // detachなしの再upsertではリンク保持を確認
            let again = repos
                .upsert_files_batch(&[upsert(root.id, "b.png", 2, 2)])
                .unwrap();
            assert_eq!(again[0].id, records[1].id);

            // 空バッチはno-op
            assert!(repos.upsert_files_batch(&[]).unwrap().is_empty());
        });
    }

    #[test]
    fn relocate_moves_path_keeps_uuid_and_asset() {
        with_repos("relocate", |repos| {
            let root = repos.add_root(Path::new("/data/p")).unwrap();
            let file = repos
                .upsert_file(upsert(root.id, "old/a.png", 5, 5))
                .unwrap();
            let asset = repos.create_asset(new_asset("rel", 5)).unwrap();
            repos.link_file(file.id, asset.id).unwrap();

            let moved = repos
                .relocate_file(file.id, upsert(root.id, "new/b.png", 5, 5))
                .unwrap();
            assert_eq!(moved.id, file.id);
            assert_eq!(moved.uuid, file.uuid, "同一実体");
            assert_eq!(moved.asset_id, Some(asset.id), "リンク保持");
            assert_eq!(moved.rel_path, PathBuf::from("new/b.png"));
            assert!(repos
                .find_by_path(root.id, &PathBuf::from("old/a.png"))
                .unwrap()
                .is_none());

            // 移動先に既存行 → Duplicate
            let other = repos
                .upsert_file(upsert(root.id, "new/c.png", 1, 1))
                .unwrap();
            assert_eq!(
                repos
                    .relocate_file(other.id, upsert(root.id, "new/b.png", 1, 1))
                    .unwrap_err(),
                RepoError::Duplicate
            );
            assert_eq!(
                repos
                    .relocate_file(9999, upsert(root.id, "x.png", 1, 1))
                    .unwrap_err(),
                RepoError::NotFound
            );
        });
    }

    #[test]
    fn scan_state_batch_counts_only_real_changes() {
        with_repos("statebatch", |repos| {
            let root = repos.add_root(Path::new("/data/p")).unwrap();
            let ids: Vec<i64> = (0..4)
                .map(|i| {
                    repos
                        .upsert_file(upsert(root.id, &format!("f{i}.png"), 1, 1))
                        .unwrap()
                        .id
                })
                .collect();
            // 1件だけ事前にmissingへ
            repos.set_scan_state(ids[3], ScanState::Missing).unwrap();

            let changed = repos
                .set_scan_state_batch(&ids, ScanState::Missing)
                .unwrap();
            assert_eq!(changed, 3, "既にmissingの1件は数えない");
            assert_eq!(repos.list_missing(root.id).unwrap().len(), 4);
            assert!(repos.set_scan_state_batch(&[], ScanState::Present).unwrap() == 0);
        });
    }

    #[test]
    fn list_files_paged_walks_all_pages() {
        with_repos("paged", |repos| {
            let root = repos.add_root(Path::new("/data/p")).unwrap();
            for i in 0..250 {
                repos
                    .upsert_file(upsert(root.id, &format!("f{i:03}.png"), 1, 1))
                    .unwrap();
            }
            let (page, total) = repos.list_files_paged(root.id, 0, 100).unwrap();
            assert_eq!(total, 250);
            assert_eq!(page.len(), 100);
            assert_eq!(page[0].rel_path, PathBuf::from("f000.png"));
            let (page, _) = repos.list_files_paged(root.id, 200, 100).unwrap();
            assert_eq!(page.len(), 50);
            let (page, _) = repos.list_files_paged(root.id, 250, 100).unwrap();
            assert!(page.is_empty());
            // 他ルートは混入しない
            let other = repos.add_root(Path::new("/data/q")).unwrap();
            let (_, other_total) = repos.list_files_paged(other.id, 0, 10).unwrap();
            assert_eq!(other_total, 0);
        });
    }

    #[test]
    fn list_present_joined_and_without_asset() {
        with_repos("joined", |repos| {
            let root = repos.add_root(Path::new("/data/p")).unwrap();
            let linked = repos
                .upsert_file(upsert(root.id, "linked.png", 1, 1))
                .unwrap();
            let asset = repos
                .create_asset(NewAsset {
                    content_hash: "blake3:jj".to_string(),
                    hash_algorithm: "blake3".to_string(),
                    byte_size: 1,
                    mime_type: Some("image/png".to_string()),
                    width: Some(640),
                    height: Some(480),
                    has_alpha: Some(true),
                    is_animated: Some(false),
                    frame_count: None,
                })
                .unwrap();
            assert_eq!(asset.width, Some(640));
            assert_eq!(asset.has_alpha, Some(true));
            assert_eq!(asset.is_animated, Some(false));
            repos.link_file(linked.id, asset.id).unwrap();
            repos
                .upsert_file(upsert(root.id, "unlinked.png", 2, 2))
                .unwrap();
            let mut missing = upsert(root.id, "missing.png", 3, 3);
            missing.scan_state = ScanState::Missing;
            repos.upsert_file(missing).unwrap();

            let joined = repos.list_present_joined().unwrap();
            assert_eq!(joined.len(), 2, "missingは除外");
            let (file, asset_opt) = joined
                .iter()
                .find(|(f, _)| f.rel_path == *"linked.png")
                .unwrap();
            let asset = asset_opt.as_ref().expect("JOINでAsset取得");
            assert_eq!(asset.width, Some(640));
            assert_eq!(asset.height, Some(480));
            assert_eq!(file.asset_id, Some(asset.id));
            let unlinked = joined
                .iter()
                .find(|(f, _)| f.rel_path == *"unlinked.png")
                .unwrap();
            assert!(unlinked.1.is_none());

            let without = repos.list_files_without_asset(root.id).unwrap();
            assert_eq!(without.len(), 1);
            assert_eq!(without[0].rel_path, PathBuf::from("unlinked.png"));
        });
    }

    #[test]
    fn create_or_get_is_idempotent_for_same_hash() {
        with_repos("corget", |repos| {
            let (a, created_a) = repos.create_or_get_asset(new_asset("cg", 1)).unwrap();
            assert!(created_a);
            let (b, created_b) = repos.create_or_get_asset(new_asset("cg", 1)).unwrap();
            assert!(!created_b);
            assert_eq!(a.id, b.id, "同一ハッシュは既存を返す");
        });
    }

    // ===== WP-7: FileOpsRepo (file_operations) =====

    use gento_core::file_ops::{FileOpKind, FileOpStatus, FileOpsRepo, NewFileOp};

    fn new_op(kind: FileOpKind, status: FileOpStatus, undoable: bool) -> NewFileOp {
        NewFileOp {
            operation_type: kind,
            status,
            undoable,
            request_json: r#"{"kind":"copy","files":[]}"#.to_string(),
        }
    }

    #[test]
    fn file_operations_round_trip_and_update() {
        with_repos("fileops", |repos| {
            let record = repos
                .insert_operation(new_op(FileOpKind::Move, FileOpStatus::Executing, false))
                .unwrap();
            assert_eq!(record.operation_type, FileOpKind::Move);
            assert_eq!(record.status, FileOpStatus::Executing);
            assert!(!record.undoable);
            assert_eq!(record.result_json, None);
            assert_eq!(record.uuid.len(), 36);

            // executingは異常終了検出で拾える
            assert_eq!(repos.list_executing_operations().unwrap().len(), 1);

            // status/result/undoable更新 (操作ログ確定)
            repos
                .update_operation(
                    record.id,
                    FileOpStatus::Partial,
                    Some(r#"{"steps":[]}"#),
                    true,
                )
                .unwrap();
            let updated = repos.get_operation(record.id).unwrap().unwrap();
            assert_eq!(updated.status, FileOpStatus::Partial);
            assert!(updated.undoable);
            assert_eq!(updated.result_json.as_deref(), Some(r#"{"steps":[]}"#));
            // executing一覧からは消える
            assert!(repos.list_executing_operations().unwrap().is_empty());

            // undo済み遷移
            repos.mark_undone(record.id).unwrap();
            let undone = repos.get_operation(record.id).unwrap().unwrap();
            assert_eq!(undone.status, FileOpStatus::Undone);
            assert!(!undone.undoable);
            assert!(undone.undone_at.is_some());

            // 不存在idはNotFound
            assert_eq!(
                repos.update_operation(9999, FileOpStatus::Done, None, false),
                Err(RepoError::NotFound)
            );
        });
    }

    #[test]
    fn file_operations_list_recent_orders_desc() {
        with_repos("fileops-list", |repos| {
            for i in 0..5 {
                repos
                    .insert_operation(new_op(FileOpKind::Trash, FileOpStatus::Done, false))
                    .unwrap();
                let _ = i;
            }
            let recent = repos.list_recent_operations(3).unwrap();
            assert_eq!(recent.len(), 3);
            assert!(
                recent[0].id > recent[1].id && recent[1].id > recent[2].id,
                "新着順"
            );
        });
    }

    #[test]
    fn file_operations_status_check_rejects_unknown_value() {
        with_repos("fileops-check", |repos| {
            let record = repos
                .insert_operation(new_op(FileOpKind::Copy, FileOpStatus::Planned, false))
                .unwrap();
            // migration #1の旧値 'pending' は #5のCHECKで拒否される
            let err = repos
                .conn()
                .execute(
                    "UPDATE file_operations SET status = 'pending' WHERE id = ?1",
                    [record.id],
                )
                .unwrap_err();
            assert!(err.to_string().contains("CHECK"), "{err}");
        });
    }

    #[test]
    fn migration_v5_maps_legacy_status_values() {
        // v4 DBに旧status値の行がある状態からv5へ移行すると新値へ写る
        let tmp = TempDir::new("v5legacy");
        let db_path = tmp.0.join("db.sqlite3");
        {
            let mut conn = Connection::open(&db_path).unwrap();
            conn.pragma_update(None, "foreign_keys", true).unwrap();
            run_migrations(&mut conn, &MIGRATIONS[..4], &MigrationOpts::default()).unwrap();
            for (i, legacy) in ["pending", "succeeded", "partial", "undone"]
                .iter()
                .enumerate()
            {
                conn.execute(
                    "INSERT INTO file_operations (uuid, operation_type, status, request_json, created_at)
                     VALUES (?1, 'copy', ?2, '{}', '2026-08-31T00:00:00.000Z')",
                    params![
                        format!("00000000-0000-7000-8000-{i:012}"),
                        legacy
                    ],
                )
                .unwrap();
            }
        }
        // v5まで適用
        {
            let mut conn = Connection::open(&db_path).unwrap();
            run_migrations(&mut conn, MIGRATIONS, &MigrationOpts::default()).unwrap();
            conn.pragma_update(None, "foreign_keys", true).unwrap();
            let repos = SqliteRepos::new(&conn);
            let all = repos.list_recent_operations(10).unwrap();
            assert_eq!(all.len(), 4);
            let by_status: std::collections::HashMap<_, _> =
                all.into_iter().map(|r| (r.id, r.status)).collect();
            let expected = [
                (1, FileOpStatus::Executing), // pending → executing
                (2, FileOpStatus::Done),      // succeeded → done
                (3, FileOpStatus::Partial),
                (4, FileOpStatus::Undone),
            ];
            for (id, status) in expected {
                assert_eq!(by_status.get(&id), Some(&status), "id={id}");
            }
        }
    }
}
