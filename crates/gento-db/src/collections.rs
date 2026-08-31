//! コレクションrepositoryのSQLite実装 (MVP WP-6)。
//!
//! [`SqliteRepos`] へ [`gento_core::collections::CollectionRepo`] を実装する。
//!
//! # スマートコレクション評価のSQL構築規約
//!
//! 条件→SQLは**固定断片 + パラメータバインドのみ**で構成する
//! (ユーザー入力がSQL文字列へ連結される経路は存在しない — 受入条件4。
//! `smart_where` が唯一の構築点で、断片は静的文字列リテラル)。
//!
//! # position維持
//!
//! 常時0開始連番 (gento-core collectionsモジュールdocの規約)。
//! 追加=末尾・削除=詰め・reorder=全順序振り直し。

use gento_core::collections::{
    CollectionItemRecord, CollectionKind, CollectionRecord, CollectionRepo, SmartCondition,
    SmartQuery,
};
use gento_core::repo::RepoError;
use rusqlite::types::Value as SqlValue;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};

use crate::ids::{new_uuid_v7, now_utc_rfc3339};
use crate::repos::{map_asset_prefixed, map_err, map_file_prefixed, SqliteRepos};
use gento_core::collections::validate_collection_name;

/// collections行のSELECT列 + manual件数 (サブクエリ。smartは後から評価で上書き)。
const COLLECTION_COLUMNS: &str = "id, uuid, name, kind, query_json, created_at, updated_at, \
     (SELECT count(*) FROM collection_items ci WHERE ci.collection_id = c.id)";

const ITEM_COLUMNS: &str = "collection_id, asset_id, position, note, created_at";

/// IN句1チャンクの最大サイズ (SQLiteの変数上限に対する安全域)。
const ASSET_ID_CHUNK: usize = 500;

fn map_collection(row: &rusqlite::Row<'_>) -> rusqlite::Result<CollectionRecord> {
    let kind_str: String = row.get(3)?;
    let kind = CollectionKind::from_db_value(&kind_str).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            format!("unknown collections.kind: {kind_str}").into(),
        )
    })?;
    Ok(CollectionRecord {
        id: row.get(0)?,
        uuid: row.get(1)?,
        name: row.get(2)?,
        kind,
        query_json: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
        item_count: row.get::<_, i64>(7)?.max(0) as u64,
    })
}

fn map_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<CollectionItemRecord> {
    Ok(CollectionItemRecord {
        collection_id: row.get(0)?,
        asset_id: row.get(1)?,
        position: row.get::<_, i64>(2)?.max(0) as u32,
        note: row.get(3)?,
        created_at: row.get(4)?,
    })
}

fn get_collection_row(conn: &Connection, id: i64) -> Result<Option<CollectionRecord>, RepoError> {
    let sql = format!("SELECT {COLLECTION_COLUMNS} FROM collections c WHERE c.id = ?1");
    conn.query_row(&sql, [id], map_collection)
        .optional()
        .map_err(map_err)
}

/// 条件列 → (WHERE句 " AND " 結合, バインド値列)。
/// 断片はすべて静的リテラル — ユーザー入力は値側のみ。
fn smart_where(conditions: &[SmartCondition]) -> (String, Vec<SqlValue>) {
    let mut fragments: Vec<&'static str> = Vec::with_capacity(conditions.len());
    let mut values: Vec<SqlValue> = Vec::with_capacity(conditions.len());
    for condition in conditions {
        match condition {
            SmartCondition::RatingGe { value } => {
                fragments.push("a.rating >= ?");
                values.push(SqlValue::Integer(*value as i64));
            }
            SmartCondition::CullState { value } => {
                fragments.push("a.triage = ?");
                values.push(SqlValue::Text(value.as_str().to_string()));
            }
            SmartCondition::Favorite { value } => {
                fragments.push("a.favorite = ?");
                values.push(SqlValue::Integer(i64::from(*value)));
            }
            SmartCondition::MimeType { value } => {
                fragments.push("a.mime_type = ?");
                values.push(SqlValue::Text(value.clone()));
            }
            SmartCondition::HasTag { name } => {
                fragments.push(
                    "EXISTS (SELECT 1 FROM asset_tags at2 \
                     JOIN tags t2 ON t2.id = at2.tag_id \
                     WHERE at2.asset_id = a.id AND t2.normalized_name = ?)",
                );
                values.push(SqlValue::Text(name.clone()));
            }
        }
    }
    if fragments.is_empty() {
        (String::new(), values)
    } else {
        (format!(" WHERE {}", fragments.join(" AND ")), values)
    }
}

/// スマートコレクションの保存済み条件を読む (JSON不正はConstraint)。
fn stored_query(record: &CollectionRecord) -> Result<SmartQuery, RepoError> {
    let json = record
        .query_json
        .as_deref()
        .ok_or_else(|| RepoError::Constraint("smart collection has no query_json".to_string()))?;
    SmartQuery::parse(json).map_err(|e| RepoError::Constraint(e.0))
}

/// smart件数 (評価)。manual行へ呼ぶとConstraint。
fn smart_count(conn: &Connection, record: &CollectionRecord) -> Result<u64, RepoError> {
    let query = stored_query(record)?;
    let (where_clause, values) = smart_where(&query.conditions);
    let sql = format!("SELECT count(*) FROM assets a{where_clause}");
    let count: i64 = conn
        .query_row(&sql, params_from_iter(values), |r| r.get(0))
        .map_err(map_err)?;
    Ok(count.max(0) as u64)
}

/// コレクション行へ件数を補完する (smart=評価値。不正JSONは0扱い —
/// 詳細エラーはevaluate/getが返す)。
fn complete_count(conn: &Connection, mut record: CollectionRecord) -> CollectionRecord {
    if record.kind == CollectionKind::Smart {
        record.item_count = smart_count(conn, &record).unwrap_or(0);
    }
    record
}

fn insert_collection(
    conn: &Connection,
    name: &str,
    kind: CollectionKind,
    query_json: Option<&str>,
) -> Result<i64, RepoError> {
    let now = now_utc_rfc3339();
    conn.execute(
        "INSERT INTO collections (uuid, name, kind, query_json, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
        params![new_uuid_v7(), name, kind.as_str(), query_json, now],
    )
    .map_err(map_err)?;
    Ok(conn.last_insert_rowid())
}

/// スマートコレクション書き込み共通: 名前検証 + queryの正規化
/// (has_tag名。保存JSONを常に正規形にする)。
fn validate_smart_input(name: &str, query: &SmartQuery) -> Result<(String, SmartQuery), RepoError> {
    let name = validate_collection_name(name)?;
    let canonical = query
        .canonicalize()
        .map_err(|e| RepoError::Constraint(e.0))?;
    Ok((name, canonical))
}

impl CollectionRepo for SqliteRepos<'_> {
    fn create_manual_collection(&self, name: &str) -> Result<CollectionRecord, RepoError> {
        let name = validate_collection_name(name)?;
        let id = insert_collection(self.conn(), &name, CollectionKind::Manual, None)?;
        get_collection_row(self.conn(), id)?
            .ok_or_else(|| RepoError::Db("inserted collection row is not readable".to_string()))
    }

    fn create_smart_collection(
        &self,
        name: &str,
        query: &SmartQuery,
    ) -> Result<CollectionRecord, RepoError> {
        let (name, canonical) = validate_smart_input(name, query)?;
        let id = insert_collection(
            self.conn(),
            &name,
            CollectionKind::Smart,
            Some(&canonical.to_json()),
        )?;
        let record = get_collection_row(self.conn(), id)?
            .ok_or_else(|| RepoError::Db("inserted collection row is not readable".to_string()))?;
        Ok(complete_count(self.conn(), record))
    }

    fn rename_collection(
        &self,
        collection_id: i64,
        name: &str,
    ) -> Result<CollectionRecord, RepoError> {
        let name = validate_collection_name(name)?;
        let changed = self
            .conn()
            .execute(
                "UPDATE collections SET name = ?2, updated_at = ?3 WHERE id = ?1",
                params![collection_id, name, now_utc_rfc3339()],
            )
            .map_err(map_err)?;
        if changed == 0 {
            return Err(RepoError::NotFound);
        }
        let record = get_collection_row(self.conn(), collection_id)?.ok_or(RepoError::NotFound)?;
        Ok(complete_count(self.conn(), record))
    }

    fn delete_collection(&self, collection_id: i64) -> Result<(), RepoError> {
        let changed = self
            .conn()
            .execute("DELETE FROM collections WHERE id = ?1", [collection_id])
            .map_err(map_err)?;
        if changed == 0 {
            return Err(RepoError::NotFound);
        }
        Ok(())
    }

    fn list_collections(&self) -> Result<Vec<CollectionRecord>, RepoError> {
        let sql = format!("SELECT {COLLECTION_COLUMNS} FROM collections c ORDER BY c.id ASC");
        let mut stmt = self.conn().prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map([], map_collection)
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows
            .into_iter()
            .map(|record| complete_count(self.conn(), record))
            .collect())
    }

    fn get_collection(&self, collection_id: i64) -> Result<Option<CollectionRecord>, RepoError> {
        match get_collection_row(self.conn(), collection_id)? {
            None => Ok(None),
            Some(record) => Ok(Some(complete_count(self.conn(), record))),
        }
    }

    fn add_collection_items(
        &self,
        collection_id: i64,
        asset_ids: &[i64],
    ) -> Result<Vec<CollectionItemRecord>, RepoError> {
        if asset_ids.is_empty() {
            return Ok(Vec::new());
        }
        let tx = self
            .conn()
            .unchecked_transaction()
            .map_err(|e| RepoError::Db(e.to_string()))?;
        let out = {
            let collection = get_collection_row(&tx, collection_id)?.ok_or(RepoError::NotFound)?;
            if collection.kind != CollectionKind::Manual {
                return Err(RepoError::Constraint(
                    "cannot add items to a smart collection".to_string(),
                ));
            }
            // Asset不在はバッチ全体をロールバック (部分適用を作らない)
            {
                let mut check = tx
                    .prepare("SELECT 1 FROM assets WHERE id = ?1")
                    .map_err(map_err)?;
                for asset_id in asset_ids {
                    check
                        .query_row([asset_id], |_| Ok(()))
                        .optional()
                        .map_err(map_err)?
                        .ok_or(RepoError::NotFound)?;
                }
            }
            let now = now_utc_rfc3339();
            let mut next_position: i64 = tx
                .query_row(
                    "SELECT COALESCE(MAX(position) + 1, 0) FROM collection_items \
                     WHERE collection_id = ?1",
                    [collection_id],
                    |r| r.get(0),
                )
                .map_err(map_err)?;
            {
                let mut stmt = tx
                    .prepare(
                        "INSERT OR IGNORE INTO collection_items \
                             (collection_id, asset_id, position, note, created_at) \
                         VALUES (?1, ?2, ?3, NULL, ?4)",
                    )
                    .map_err(map_err)?;
                for asset_id in asset_ids {
                    let changed = stmt
                        .execute(params![collection_id, asset_id, next_position, now])
                        .map_err(map_err)?;
                    // 既存行 (冪等スキップ) ではpositionを進めない — 連番維持
                    if changed > 0 {
                        next_position += 1;
                    }
                }
            }
            read_items(&tx, collection_id, asset_ids)?
        };
        tx.commit().map_err(map_err)?;
        Ok(out)
    }

    fn remove_collection_item(&self, collection_id: i64, asset_id: i64) -> Result<(), RepoError> {
        let tx = self
            .conn()
            .unchecked_transaction()
            .map_err(|e| RepoError::Db(e.to_string()))?;
        {
            let changed = tx
                .execute(
                    "DELETE FROM collection_items WHERE collection_id = ?1 AND asset_id = ?2",
                    params![collection_id, asset_id],
                )
                .map_err(map_err)?;
            if changed == 0 {
                return Err(RepoError::NotFound);
            }
            // 残項目を0開始連番へ詰める (position規約)
            tx.execute(
                "WITH ordered AS (
                     SELECT asset_id, ROW_NUMBER() OVER (ORDER BY position) - 1 AS newpos
                     FROM collection_items WHERE collection_id = ?1
                 )
                 UPDATE collection_items SET position = (
                     SELECT newpos FROM ordered WHERE ordered.asset_id = collection_items.asset_id
                 ) WHERE collection_id = ?1",
                [collection_id],
            )
            .map_err(map_err)?;
        }
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    fn reorder_collection(
        &self,
        collection_id: i64,
        asset_ids: &[i64],
    ) -> Result<Vec<CollectionItemRecord>, RepoError> {
        let tx = self
            .conn()
            .unchecked_transaction()
            .map_err(|e| RepoError::Db(e.to_string()))?;
        let out = {
            let collection = get_collection_row(&tx, collection_id)?.ok_or(RepoError::NotFound)?;
            if collection.kind != CollectionKind::Manual {
                return Err(RepoError::Constraint(
                    "cannot reorder a smart collection".to_string(),
                ));
            }
            let current: Vec<i64> = {
                let mut stmt = tx
                    .prepare(
                        "SELECT asset_id FROM collection_items WHERE collection_id = ?1 \
                         ORDER BY position",
                    )
                    .map_err(map_err)?;
                let rows = stmt
                    .query_map([collection_id], |row| row.get(0))
                    .map_err(map_err)?
                    .collect::<rusqlite::Result<Vec<i64>>>()
                    .map_err(map_err)?;
                rows
            };
            // 全順序送り: 入力は現在の項目集合と完全一致 (重複・過不足不可)
            let mut sorted_input = asset_ids.to_vec();
            sorted_input.sort_unstable();
            let mut sorted_current = current;
            sorted_current.sort_unstable();
            if sorted_input != sorted_current {
                return Err(RepoError::Constraint(
                    "reorder must list every item of the collection exactly once".to_string(),
                ));
            }
            {
                let mut stmt = tx
                    .prepare(
                        "UPDATE collection_items SET position = ?3 \
                         WHERE collection_id = ?1 AND asset_id = ?2",
                    )
                    .map_err(map_err)?;
                for (index, asset_id) in asset_ids.iter().enumerate() {
                    stmt.execute(params![collection_id, asset_id, index as i64])
                        .map_err(map_err)?;
                }
            }
            let all: Vec<i64> = asset_ids.to_vec();
            read_items(&tx, collection_id, &all)?
        };
        tx.commit().map_err(map_err)?;
        Ok(out)
    }

    fn list_collection_items(
        &self,
        collection_id: i64,
        offset: u32,
        limit: u32,
    ) -> Result<(Vec<CollectionItemRecord>, u64), RepoError> {
        let tx = self
            .conn()
            .unchecked_transaction()
            .map_err(|e| RepoError::Db(e.to_string()))?;
        let total: i64 = tx
            .query_row(
                "SELECT count(*) FROM collection_items WHERE collection_id = ?1",
                [collection_id],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        let sql = format!(
            "SELECT {ITEM_COLUMNS} FROM collection_items \
             WHERE collection_id = ?1 ORDER BY position ASC LIMIT ?2 OFFSET ?3"
        );
        let mut stmt = tx.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(params![collection_id, limit, offset], map_item)
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        drop(stmt);
        tx.commit().map_err(map_err)?;
        Ok((rows, total.max(0) as u64))
    }

    fn set_collection_item_note(
        &self,
        collection_id: i64,
        asset_id: i64,
        note: Option<&str>,
    ) -> Result<CollectionItemRecord, RepoError> {
        let changed = self
            .conn()
            .execute(
                "UPDATE collection_items SET note = ?3 \
                 WHERE collection_id = ?1 AND asset_id = ?2",
                params![collection_id, asset_id, note],
            )
            .map_err(map_err)?;
        if changed == 0 {
            return Err(RepoError::NotFound);
        }
        let sql = format!(
            "SELECT {ITEM_COLUMNS} FROM collection_items \
             WHERE collection_id = ?1 AND asset_id = ?2"
        );
        self.conn()
            .query_row(&sql, params![collection_id, asset_id], map_item)
            .optional()
            .map_err(map_err)?
            .ok_or(RepoError::NotFound)
    }

    fn update_smart_collection(
        &self,
        collection_id: i64,
        name: &str,
        query: &SmartQuery,
    ) -> Result<CollectionRecord, RepoError> {
        let (name, canonical) = validate_smart_input(name, query)?;
        let record = get_collection_row(self.conn(), collection_id)?.ok_or(RepoError::NotFound)?;
        if record.kind != CollectionKind::Smart {
            return Err(RepoError::Constraint(
                "cannot update query of a manual collection".to_string(),
            ));
        }
        self.conn()
            .execute(
                "UPDATE collections SET name = ?2, query_json = ?3, updated_at = ?4 WHERE id = ?1",
                params![collection_id, name, canonical.to_json(), now_utc_rfc3339()],
            )
            .map_err(map_err)?;
        let updated = get_collection_row(self.conn(), collection_id)?.ok_or(RepoError::NotFound)?;
        Ok(complete_count(self.conn(), updated))
    }

    fn evaluate_smart_collection(
        &self,
        collection_id: i64,
        offset: u32,
        limit: u32,
    ) -> Result<(Vec<i64>, u64), RepoError> {
        let record = get_collection_row(self.conn(), collection_id)?.ok_or(RepoError::NotFound)?;
        if record.kind != CollectionKind::Smart {
            return Err(RepoError::Constraint(
                "cannot evaluate a manual collection".to_string(),
            ));
        }
        let query = stored_query(&record)?;
        let (where_clause, values) = smart_where(&query.conditions);
        let tx = self
            .conn()
            .unchecked_transaction()
            .map_err(|e| RepoError::Db(e.to_string()))?;
        let total: i64 = tx
            .query_row(
                &format!("SELECT count(*) FROM assets a{where_clause}"),
                params_from_iter(values.iter()),
                |r| r.get(0),
            )
            .map_err(map_err)?;
        let mut page_values = values;
        page_values.push(SqlValue::Integer(limit as i64));
        page_values.push(SqlValue::Integer(offset as i64));
        let sql =
            format!("SELECT a.id FROM assets a{where_clause} ORDER BY a.id ASC LIMIT ? OFFSET ?");
        let mut stmt = tx.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(params_from_iter(page_values.iter()), |row| row.get(0))
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<i64>>>()
            .map_err(map_err)?;
        drop(stmt);
        tx.commit().map_err(map_err)?;
        Ok((rows, total.max(0) as u64))
    }

    fn list_present_files_of_assets(
        &self,
        asset_ids: &[i64],
    ) -> Result<Vec<(gento_core::repo::FileRecord, gento_core::repo::AssetRecord)>, RepoError> {
        // 入力の重複を最初出現位置で一意化 (順序保持)
        let mut unique: Vec<i64> = Vec::with_capacity(asset_ids.len());
        let mut seen = std::collections::HashSet::new();
        for id in asset_ids {
            if seen.insert(*id) {
                unique.push(*id);
            }
        }
        let mut by_asset: std::collections::HashMap<
            i64,
            (gento_core::repo::FileRecord, gento_core::repo::AssetRecord),
        > = std::collections::HashMap::with_capacity(unique.len());
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
             FROM files f JOIN assets a ON f.asset_id = a.id
             WHERE f.scan_state = 'present' AND a.id IN ({placeholders})
             ORDER BY f.id ASC";
        for chunk in unique.chunks(ASSET_ID_CHUNK) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let sql = SQL.replace("{placeholders}", &placeholders);
            let mut stmt = self.conn().prepare(&sql).map_err(map_err)?;
            let rows = stmt
                .query_map(params_from_iter(chunk.iter()), |row| {
                    Ok((map_file_prefixed(row)?, map_asset_prefixed(row)?))
                })
                .map_err(map_err)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(map_err)?;
            for (file, asset) in rows {
                // ORDER BY f.id ASC のため先頭が代表file (最小file id)
                by_asset.entry(asset.id).or_insert((file, asset));
            }
        }
        Ok(unique
            .into_iter()
            .filter_map(|id| by_asset.remove(&id))
            .collect())
    }
}

/// 要求asset列の項目を行IDで読み戻す (tx内)。
fn read_items(
    conn: &Connection,
    collection_id: i64,
    asset_ids: &[i64],
) -> Result<Vec<CollectionItemRecord>, RepoError> {
    let sql = format!(
        "SELECT {ITEM_COLUMNS} FROM collection_items \
         WHERE collection_id = ?1 AND asset_id = ?2"
    );
    let mut stmt = conn.prepare(&sql).map_err(map_err)?;
    let mut out = Vec::with_capacity(asset_ids.len());
    for asset_id in asset_ids {
        if let Some(record) = stmt
            .query_row(params![collection_id, asset_id], map_item)
            .optional()
            .map_err(map_err)?
        {
            out.push(record);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrate::{run_migrations, MigrationOpts};
    use crate::schema::MIGRATIONS;
    use gento_core::collections::SmartCullValue;
    use gento_core::metadata::{AssetTagRepo, CaptionKind, CaptionRepo};
    use gento_core::repo::{
        AssetRepo, FileRepo, FileUpsert, LibraryRootRepo, NewAsset, ScanState, TriageState,
    };
    use std::path::{Path, PathBuf};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "gento-db-coll-{tag}-{}-{}",
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

    fn with_repos<T>(tag: &str, f: impl FnOnce(&SqliteRepos<'_>) -> T) -> T {
        let tmp = TempDir::new(tag);
        let mut conn = Connection::open(tmp.0.join("db.sqlite3")).unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        run_migrations(&mut conn, MIGRATIONS, &MigrationOpts::default()).unwrap();
        let repos = SqliteRepos::new(&conn);
        f(&repos)
    }

    fn make_asset(repos: &SqliteRepos<'_>, hash: &str) -> i64 {
        repos
            .create_asset(NewAsset {
                content_hash: format!("blake3:{hash}"),
                hash_algorithm: "blake3".to_string(),
                byte_size: 10,
                mime_type: None,
                width: None,
                height: None,
                has_alpha: None,
                is_animated: None,
                frame_count: None,
            })
            .unwrap()
            .id
    }

    fn make_mime_asset(repos: &SqliteRepos<'_>, hash: &str, mime: &str) -> i64 {
        repos
            .create_asset(NewAsset {
                content_hash: format!("blake3:{hash}"),
                hash_algorithm: "blake3".to_string(),
                byte_size: 10,
                mime_type: Some(mime.to_string()),
                width: None,
                height: None,
                has_alpha: None,
                is_animated: None,
                frame_count: None,
            })
            .unwrap()
            .id
    }

    fn link_present_file(repos: &SqliteRepos<'_>, root_id: i64, rel: &str, asset_id: i64) -> i64 {
        let file = repos
            .upsert_file(FileUpsert {
                root_id,
                rel_path: PathBuf::from(rel),
                display_name: rel.to_string(),
                byte_size: 10,
                mtime_ms: 1,
                mtime_ns: 1_000,
                scan_state: ScanState::Present,
                os_file_id: None,
                detach_asset: false,
            })
            .unwrap();
        repos.link_file(file.id, asset_id).unwrap();
        file.id
    }

    fn query(conditions: Vec<SmartCondition>) -> SmartQuery {
        SmartQuery { conditions }
    }

    // ===== manual CRUD・同名許可 =====

    #[test]
    fn manual_crud_and_duplicate_names_allowed() {
        with_repos("crud", |repos| {
            let c1 = repos.create_manual_collection(" 選択 ").unwrap();
            assert_eq!(c1.name, "選択");
            assert_eq!(c1.kind, CollectionKind::Manual);
            assert_eq!(c1.item_count, 0);
            assert!(c1.uuid.len() == 36);

            // 同名許可 (uuidで識別 — WP-6確定方針)
            let c2 = repos.create_manual_collection("選択").unwrap();
            assert_ne!(c1.id, c2.id);
            assert_ne!(c1.uuid, c2.uuid);

            // 空名拒否
            assert!(matches!(
                repos.create_manual_collection("  "),
                Err(RepoError::Constraint(_))
            ));

            // 改名
            let renamed = repos.rename_collection(c1.id, "best").unwrap();
            assert_eq!(renamed.name, "best");
            assert_eq!(
                repos.rename_collection(9999, "x").unwrap_err(),
                RepoError::NotFound
            );

            // 一覧 (作成順)・件数付き
            let list = repos.list_collections().unwrap();
            assert_eq!(list.len(), 2);
            assert_eq!(list[0].id, c1.id);

            // get
            assert_eq!(repos.get_collection(c2.id).unwrap().unwrap().name, "選択");
            assert!(repos.get_collection(9999).unwrap().is_none());

            // 削除 (itemsごと)
            let a = make_asset(repos, "d1");
            repos.add_collection_items(c2.id, &[a]).unwrap();
            repos.delete_collection(c2.id).unwrap();
            assert!(repos.get_collection(c2.id).unwrap().is_none());
            let items = repos.list_collection_items(c2.id, 0, 10).unwrap();
            assert_eq!(items.0.len(), 0, "itemsも削除 (CASCADE)");
            assert_eq!(
                repos.delete_collection(c2.id).unwrap_err(),
                RepoError::NotFound
            );
        });
    }

    // ===== position一貫性 =====

    #[test]
    fn positions_append_remove_compact_and_reorder() {
        with_repos("position", |repos| {
            let c = repos.create_manual_collection("pos").unwrap();
            let a1 = make_asset(repos, "p1");
            let a2 = make_asset(repos, "p2");
            let a3 = make_asset(repos, "p3");
            let a4 = make_asset(repos, "p4");

            // 追加は末尾 (0,1,2)
            repos.add_collection_items(c.id, &[a1, a2, a3]).unwrap();
            let (items, total) = repos.list_collection_items(c.id, 0, 100).unwrap();
            assert_eq!(total, 3);
            assert_eq!(
                items
                    .iter()
                    .map(|i| (i.asset_id, i.position))
                    .collect::<Vec<_>>(),
                vec![(a1, 0), (a2, 1), (a3, 2)]
            );

            // 冪等再追加 (position保持・件数不変)
            repos.add_collection_items(c.id, &[a2, a4]).unwrap();
            let (items, total) = repos.list_collection_items(c.id, 0, 100).unwrap();
            assert_eq!(total, 4);
            assert_eq!(items[1].asset_id, a2);
            assert_eq!(items[1].position, 1, "既存はposition保持");
            assert_eq!(items[3].asset_id, a4);
            assert_eq!(items[3].position, 3, "新規は末尾 (連番維持)");

            // 削除後の詰め: a2を削除 → 0,1,2に詰まる
            repos.remove_collection_item(c.id, a2).unwrap();
            let (items, _) = repos.list_collection_items(c.id, 0, 100).unwrap();
            assert_eq!(
                items
                    .iter()
                    .map(|i| (i.asset_id, i.position))
                    .collect::<Vec<_>>(),
                vec![(a1, 0), (a3, 1), (a4, 2)]
            );
            assert_eq!(
                repos.remove_collection_item(c.id, a2).unwrap_err(),
                RepoError::NotFound
            );

            // reorder: 全順序送り → 厳密に振り直し
            repos.reorder_collection(c.id, &[a4, a1, a3]).unwrap();
            let (items, _) = repos.list_collection_items(c.id, 0, 100).unwrap();
            assert_eq!(
                items
                    .iter()
                    .map(|i| (i.asset_id, i.position))
                    .collect::<Vec<_>>(),
                vec![(a4, 0), (a1, 1), (a3, 2)]
            );

            // 部分順序・過不足・重複は拒否
            assert!(matches!(
                repos.reorder_collection(c.id, &[a1, a3]),
                Err(RepoError::Constraint(_))
            ));
            assert!(matches!(
                repos.reorder_collection(c.id, &[a1, a3, a4, a4]),
                Err(RepoError::Constraint(_))
            ));
            assert!(matches!(
                repos.reorder_collection(c.id, &[a1, a2, a3, a4]),
                Err(RepoError::Constraint(_))
            ));
            // 元の順序は不変 (失敗で壊さない)
            let (items, _) = repos.list_collection_items(c.id, 0, 100).unwrap();
            assert_eq!(
                items.iter().map(|i| i.asset_id).collect::<Vec<_>>(),
                vec![a4, a1, a3]
            );

            // ページング
            let (page, total) = repos.list_collection_items(c.id, 1, 1).unwrap();
            assert_eq!(total, 3);
            assert_eq!(page.len(), 1);
            assert_eq!(page[0].asset_id, a1);
        });
    }

    #[test]
    fn add_items_validates_collection_and_assets_atomically() {
        with_repos("addvalid", |repos| {
            let a1 = make_asset(repos, "v1");
            let smart = repos
                .create_smart_collection("s", &query(vec![SmartCondition::RatingGe { value: 0 }]))
                .unwrap();
            // smartへの追加は拒否
            assert!(matches!(
                repos.add_collection_items(smart.id, &[a1]),
                Err(RepoError::Constraint(_))
            ));
            // 不在コレクション
            assert_eq!(
                repos.add_collection_items(9999, &[a1]).unwrap_err(),
                RepoError::NotFound
            );
            // 不在asset混入 → ロールバック (a1も追加されない)
            let c = repos.create_manual_collection("m").unwrap();
            assert_eq!(
                repos.add_collection_items(c.id, &[a1, 9999]).unwrap_err(),
                RepoError::NotFound
            );
            let (items, total) = repos.list_collection_items(c.id, 0, 10).unwrap();
            assert_eq!((items.len(), total), (0, 0), "ロールバック済み");
            // 空リストはno-op
            assert!(repos.add_collection_items(c.id, &[]).unwrap().is_empty());
        });
    }

    #[test]
    fn item_note_set_and_clear() {
        with_repos("note", |repos| {
            let c = repos.create_manual_collection("n").unwrap();
            let a = make_asset(repos, "nt");
            repos.add_collection_items(c.id, &[a]).unwrap();
            assert_eq!(
                repos.list_collection_items(c.id, 0, 10).unwrap().0[0].note,
                None
            );
            let updated = repos
                .set_collection_item_note(c.id, a, Some("差し込み済み"))
                .unwrap();
            assert_eq!(updated.note.as_deref(), Some("差し込み済み"));
            assert_eq!(
                repos.list_collection_items(c.id, 0, 10).unwrap().0[0].note,
                Some("差し込み済み".to_string())
            );
            // クリア
            repos.set_collection_item_note(c.id, a, None).unwrap();
            assert_eq!(
                repos.list_collection_items(c.id, 0, 10).unwrap().0[0].note,
                None
            );
            assert_eq!(
                repos
                    .set_collection_item_note(c.id, 9999, Some("x"))
                    .unwrap_err(),
                RepoError::NotFound
            );
        });
    }

    // ===== smart: query_json保存・schema検証 =====

    #[test]
    fn smart_create_stores_canonical_json() {
        with_repos("smartjson", |repos| {
            let c = repos
                .create_smart_collection(
                    "高評価",
                    &query(vec![
                        SmartCondition::RatingGe { value: 4 },
                        SmartCondition::HasTag {
                            name: "Sky".to_string(),
                        },
                    ]),
                )
                .unwrap();
            assert_eq!(c.kind, CollectionKind::Smart);
            // 正規形JSON (has_tagは正規化名)
            assert_eq!(
                c.query_json.as_deref(),
                Some(
                    r#"{"conditions":[{"type":"rating_ge","value":4},{"type":"has_tag","name":"sky"}]}"#
                )
            );
            // 保存済みJSONは再解析可能 (round-trip)
            let parsed = SmartQuery::parse(c.query_json.as_deref().unwrap()).unwrap();
            assert_eq!(
                parsed.conditions[1],
                SmartCondition::HasTag {
                    name: "sky".to_string()
                }
            );
        });
    }

    #[test]
    fn smart_update_and_kind_guards() {
        with_repos("smartupd", |repos| {
            let smart = repos
                .create_smart_collection(
                    "s",
                    &query(vec![SmartCondition::Favorite { value: true }]),
                )
                .unwrap();
            let updated = repos
                .update_smart_collection(
                    smart.id,
                    "s2",
                    &query(vec![SmartCondition::CullState {
                        value: SmartCullValue::Rejected,
                    }]),
                )
                .unwrap();
            assert_eq!(updated.name, "s2");
            assert!(updated
                .query_json
                .as_deref()
                .unwrap()
                .contains("cull_state"));

            let manual = repos.create_manual_collection("m").unwrap();
            assert!(matches!(
                repos.update_smart_collection(manual.id, "x", &query(vec![])),
                Err(RepoError::Constraint(_))
            ));
            assert!(matches!(
                repos.evaluate_smart_collection(manual.id, 0, 10),
                Err(RepoError::Constraint(_))
            ));
            assert_eq!(
                repos.evaluate_smart_collection(9999, 0, 10).unwrap_err(),
                RepoError::NotFound
            );
        });
    }

    // ===== evaluate: SQLの正しさ (各条件型・複数AND・ページング) =====

    #[test]
    fn evaluate_each_condition_type() {
        with_repos("evaluate", |repos| {
            let base = make_asset(repos, "e-base"); // すべて初期値
            let rated = make_asset(repos, "e-rated");
            repos.set_rating(rated, 4).unwrap();
            let picked = make_asset(repos, "e-picked");
            repos.set_cull_state(picked, TriageState::Picked).unwrap();
            let rejected = make_asset(repos, "e-rejected");
            repos
                .set_cull_state(rejected, TriageState::Rejected)
                .unwrap();
            let faved = make_asset(repos, "e-faved");
            repos.set_favorite(faved, true).unwrap();
            let tagged = make_asset(repos, "e-tagged");
            repos.add_tags_batch(&[tagged], &["Sky"]).unwrap();
            let png = make_mime_asset(repos, "e-png", "image/png");
            let jpeg = make_mime_asset(repos, "e-jpeg", "image/jpeg");

            let eval = |conditions: Vec<SmartCondition>| -> Vec<i64> {
                let c = repos
                    .create_smart_collection("tmp", &query(conditions))
                    .unwrap();
                let (ids, total) = repos.evaluate_smart_collection(c.id, 0, 1000).unwrap();
                assert_eq!(total as usize, ids.len());
                repos.delete_collection(c.id).unwrap();
                ids
            };

            // rating_ge 4: ratedのみ
            assert_eq!(
                eval(vec![SmartCondition::RatingGe { value: 4 }]),
                vec![rated]
            );
            // rating_ge 0: 全件 (空条件と同じ)
            assert_eq!(eval(vec![SmartCondition::RatingGe { value: 0 }]).len(), 8);
            // cull_state picked / rejected
            assert_eq!(
                eval(vec![SmartCondition::CullState {
                    value: SmartCullValue::Picked
                }]),
                vec![picked]
            );
            assert_eq!(
                eval(vec![SmartCondition::CullState {
                    value: SmartCullValue::Rejected
                }]),
                vec![rejected]
            );
            // favorite
            assert_eq!(
                eval(vec![SmartCondition::Favorite { value: true }]),
                vec![faved]
            );
            assert_eq!(
                eval(vec![SmartCondition::Favorite { value: false }]).len(),
                7
            );
            // has_tag (正規化名で一致 — "Sky"入力は"sky"へ)
            assert_eq!(
                eval(vec![SmartCondition::HasTag {
                    name: "Sky".to_string()
                }]),
                vec![tagged]
            );
            assert_eq!(
                eval(vec![SmartCondition::HasTag {
                    name: "nonexistent".to_string()
                }]),
                Vec::<i64>::new()
            );
            // mime_type
            assert_eq!(
                eval(vec![SmartCondition::MimeType {
                    value: "image/png".to_string()
                }]),
                vec![png]
            );
            assert_eq!(
                eval(vec![SmartCondition::MimeType {
                    value: "image/jpeg".to_string()
                }]),
                vec![jpeg]
            );
            // 複数AND: rating>=4 かつ favorite (どちらも満たすassetなし)
            assert_eq!(
                eval(vec![
                    SmartCondition::RatingGe { value: 4 },
                    SmartCondition::Favorite { value: true }
                ]),
                Vec::<i64>::new()
            );
            // 空条件 = 全asset
            assert_eq!(eval(vec![]).len(), 8);
            let _ = base;
        });
    }

    #[test]
    fn evaluate_paging_and_id_order() {
        with_repos("paging", |repos| {
            for i in 0..5 {
                make_asset(repos, &format!("pg{i}"));
            }
            let c = repos
                .create_smart_collection("all", &query(vec![]))
                .unwrap();
            let (page1, total) = repos.evaluate_smart_collection(c.id, 0, 2).unwrap();
            let (page2, _) = repos.evaluate_smart_collection(c.id, 2, 2).unwrap();
            let (page3, _) = repos.evaluate_smart_collection(c.id, 4, 2).unwrap();
            assert_eq!(total, 5);
            assert_eq!(page1.len(), 2);
            assert_eq!(page2.len(), 2);
            assert_eq!(page3.len(), 1);
            let mut all = page1.clone();
            all.extend(page2);
            all.extend(page3);
            // id昇順・重複なし
            assert_eq!(all.len(), 5);
            assert!(all.windows(2).all(|w| w[0] < w[1]));
        });
    }

    #[test]
    fn smart_count_in_list_and_get() {
        with_repos("count", |repos| {
            let a1 = make_asset(repos, "c1");
            let a2 = make_asset(repos, "c2");
            repos.set_rating(a1, 5).unwrap();
            repos.set_rating(a2, 3).unwrap();
            let smart = repos
                .create_smart_collection("s", &query(vec![SmartCondition::RatingGe { value: 4 }]))
                .unwrap();
            assert_eq!(smart.item_count, 1, "評価件数がcreate応答に載る");
            let manual = repos.create_manual_collection("m").unwrap();
            repos.add_collection_items(manual.id, &[a1, a2]).unwrap();
            let list = repos.list_collections().unwrap();
            assert_eq!(list[0].item_count, 1, "smartは評価件数");
            assert_eq!(list[1].item_count, 2, "manualは項目数");
            assert_eq!(
                repos.get_collection(manual.id).unwrap().unwrap().item_count,
                2
            );
        });
    }

    // ===== 同一Assetを複数コレクションへ (受入条件2) =====

    #[test]
    fn same_asset_in_multiple_collections() {
        with_repos("multi", |repos| {
            let a = make_asset(repos, "shared");
            let c1 = repos.create_manual_collection("one").unwrap();
            let c2 = repos.create_manual_collection("two").unwrap();
            repos.add_collection_items(c1.id, &[a]).unwrap();
            repos.add_collection_items(c2.id, &[a]).unwrap();
            assert_eq!(repos.get_collection(c1.id).unwrap().unwrap().item_count, 1);
            assert_eq!(repos.get_collection(c2.id).unwrap().unwrap().item_count, 1);
            // 片方を削除しても他方は無事
            repos.remove_collection_item(c1.id, a).unwrap();
            assert_eq!(repos.get_collection(c2.id).unwrap().unwrap().item_count, 1);
            // Asset自体は削除されない
            assert!(repos.get_asset(a).unwrap().is_some());
        });
    }

    // ===== 表示用: present files of assets =====

    #[test]
    fn present_files_of_assets_order_and_representative() {
        with_repos("files", |repos| {
            let root = repos.add_root(Path::new("/data/p")).unwrap();
            let a1 = make_asset(repos, "f1");
            let a2 = make_asset(repos, "f2");
            let a3 = make_asset(repos, "f3-no-file");
            link_present_file(repos, root.id, "first.png", a1); // 最小file id → 代表
            link_present_file(repos, root.id, "second.png", a1); // 同一Assetの2つ目
            link_present_file(repos, root.id, "b.png", a2);
            // a3はpresentなfileを持たない

            let out = repos.list_present_files_of_assets(&[a3, a2, a1]).unwrap();
            assert_eq!(out.len(), 2, "a3は除外");
            // 入力順 (a3除去後: a2, a1)
            assert_eq!(out[0].1.id, a2);
            assert_eq!(out[0].0.display_name, "b.png");
            assert_eq!(out[1].1.id, a1);
            assert_eq!(out[1].0.display_name, "first.png", "最小file idが代表");
            // 重複入力は一意化
            let out = repos.list_present_files_of_assets(&[a1, a1]).unwrap();
            assert_eq!(out.len(), 1);
            // 空
            assert!(repos.list_present_files_of_assets(&[]).unwrap().is_empty());
        });
    }

    #[test]
    fn present_files_of_assets_handles_more_than_chunk() {
        with_repos("chunk", |repos| {
            let root = repos.add_root(Path::new("/data/p")).unwrap();
            let mut ids = Vec::new();
            for i in 0..(ASSET_ID_CHUNK as i64 + 7) {
                let a = make_asset(repos, &format!("k{i}"));
                link_present_file(repos, root.id, &format!("{i}.png"), a);
                ids.push(a);
            }
            let out = repos.list_present_files_of_assets(&ids).unwrap();
            assert_eq!(out.len(), ids.len());
            // 入力順を保持
            for (index, (_, asset)) in out.iter().enumerate() {
                assert_eq!(asset.id, ids[index]);
            }
        });
    }

    // ===== 元ファイル無操作 (受入条件1) =====

    #[test]
    fn collection_operations_never_touch_filesystem() {
        with_repos("fsnoop", |repos| {
            let root = repos.add_root(Path::new("/data/p")).unwrap();
            let a1 = make_asset(repos, "fs1");
            let a2 = make_asset(repos, "fs2");
            link_present_file(repos, root.id, "a.png", a1);
            link_present_file(repos, root.id, "b.png", a2);
            repos.set_rating(a2, 5).unwrap();

            // 実ディレクトリは存在しない — repo操作は純DB操作のため
            // ファイルシステムへ触れなくても完結する (=fs無操作の構造的証明)。
            let c = repos.create_manual_collection("m").unwrap();
            repos.add_collection_items(c.id, &[a1, a2]).unwrap();
            repos.reorder_collection(c.id, &[a2, a1]).unwrap();
            repos
                .set_collection_item_note(c.id, a1, Some("memo"))
                .unwrap();
            let smart = repos
                .create_smart_collection("s", &query(vec![SmartCondition::RatingGe { value: 4 }]))
                .unwrap();
            let (ids, _) = repos.evaluate_smart_collection(smart.id, 0, 10).unwrap();
            assert_eq!(ids, vec![a2]);
            repos.remove_collection_item(c.id, a1).unwrap();
            repos.delete_collection(c.id).unwrap();
            // DB行はそのまま
            assert!(repos.get_asset(a1).unwrap().is_some());
            assert!(repos.get_asset(a2).unwrap().is_some());
        });
    }

    #[test]
    fn smart_evaluation_after_metadata_change_tracks_db() {
        // 受入条件3のrepo層証明: メタデータ変更後の評価は即座にDB実値へ追従
        with_repos("track", |repos| {
            let a = make_asset(repos, "t1");
            let smart = repos
                .create_smart_collection("s", &query(vec![SmartCondition::RatingGe { value: 4 }]))
                .unwrap();
            let (ids, total) = repos.evaluate_smart_collection(smart.id, 0, 10).unwrap();
            assert_eq!((ids.len(), total), (0, 0));
            repos.set_rating(a, 4).unwrap();
            let (ids, total) = repos.evaluate_smart_collection(smart.id, 0, 10).unwrap();
            assert_eq!(ids, vec![a]);
            assert_eq!(total, 1);
            repos.set_rating(a, 2).unwrap();
            let (ids, total) = repos.evaluate_smart_collection(smart.id, 0, 10).unwrap();
            assert_eq!((ids.len(), total), (0, 0));
            // キャプション変更は条件に影響しない (評価対象外)
            repos.set_caption(a, CaptionKind::UserNote, "memo").unwrap();
            let (_, total) = repos.evaluate_smart_collection(smart.id, 0, 10).unwrap();
            assert_eq!(total, 0);
        });
    }
}
