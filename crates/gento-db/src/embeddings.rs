//! 埋め込みベクトルの永続化 (MVP WP-11。embeddings表 = ベクトル正本)。
//!
//! - `purpose='doc'`: 検索文書ベクトル (ruri-v3)。`dtype='f32'`、
//!   vector BLOBは **little-endian f32列** (USearch索引化はWP-12)。
//! - `source_hash`: 検索文書のBLAKE3ハッシュ。メタデータ変更で文書が
//!   変わったassetの再埋め込み判定に使う (WP-12の期限切れ検出と同一仕組み)。
//! - UNIQUE (asset_id, purpose, model_name, model_revision) へのupsert。
//!
//! 設定 (自然言語検索の有効化・選択モデル) は app_settings kvストア。

use gento_core::repo::RepoError;
use rusqlite::{params, Connection, OptionalExtension};

use crate::ids::{new_uuid_v7, now_utc_rfc3339};
use crate::repos::{map_err, SqliteRepos};

/// 埋め込みジョブ種別 (migration #9のCHECK正本)。
pub const JOB_TYPE_EMBEDDING: &str = "embedding";

/// app_settings: 自然言語検索有効フラグのキー。
pub const SETTING_EMBEDDING_ENABLED: &str = "embedding.enabled";
/// app_settings: 選択モデルIDのキー。
pub const SETTING_EMBEDDING_MODEL: &str = "embedding.model";

/// embeddings行 (ベクトル本体を除くメタ情報)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingMeta {
    pub id: i64,
    pub asset_id: i64,
    pub purpose: String,
    pub model_name: String,
    pub model_revision: String,
    pub dimensions: usize,
    pub dtype: String,
    pub source_hash: String,
    pub created_at: String,
}

/// f32列をlittle-endian BLOBへエンコード。
pub fn encode_vector_f32(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vector.len() * 4);
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

/// little-endian BLOBをf32列へデコード。長さが4の倍数でない場合はErr。
pub fn decode_vector_f32(bytes: &[u8]) -> Result<Vec<f32>, RepoError> {
    if !bytes.len().is_multiple_of(4) {
        return Err(RepoError::Db(format!(
            "ベクトルBLOBの長さが4の倍数ではありません: {}",
            bytes.len()
        )));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect())
}

impl SqliteRepos<'_> {
    /// 埋め込みをupsertする (asset_id+purpose+model_name+model_revisionで
    /// 一意。既存は更新)。引数は埋め込み行の列構成そのもの。
    #[allow(clippy::too_many_arguments)]
    pub fn embedding_upsert(
        &self,
        asset_id: i64,
        model_name: &str,
        model_revision: &str,
        dimensions: usize,
        dtype: &str,
        source_hash: &str,
        vector: &[f32],
    ) -> Result<EmbeddingMeta, RepoError> {
        let now = now_utc_rfc3339();
        let blob = encode_vector_f32(vector);
        self.conn()
            .execute(
                "INSERT INTO embeddings (uuid, asset_id, purpose, model_name, model_revision, \
                 dimensions, dtype, source_hash, vector, created_at) \
                 VALUES (?1, ?2, 'doc', ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
                 ON CONFLICT (asset_id, purpose, model_name, model_revision) DO UPDATE SET \
                 dimensions = excluded.dimensions, dtype = excluded.dtype, \
                 source_hash = excluded.source_hash, vector = excluded.vector, \
                 created_at = excluded.created_at",
                params![
                    new_uuid_v7(),
                    asset_id,
                    model_name,
                    model_revision,
                    dimensions as i64,
                    dtype,
                    source_hash,
                    blob,
                    now
                ],
            )
            .map_err(map_err)?;
        self.embedding_meta_for(asset_id, model_name, model_revision)?
            .ok_or(RepoError::NotFound)
    }

    /// 既存埋め込みのメタ情報を取得。
    pub fn embedding_meta_for(
        &self,
        asset_id: i64,
        model_name: &str,
        model_revision: &str,
    ) -> Result<Option<EmbeddingMeta>, RepoError> {
        self.conn()
            .query_row(
                "SELECT id, asset_id, purpose, model_name, model_revision, dimensions, dtype, \
                 source_hash, created_at FROM embeddings \
                 WHERE asset_id = ?1 AND purpose = 'doc' AND model_name = ?2 AND model_revision = ?3",
                params![asset_id, model_name, model_revision],
                |row| {
                    Ok(EmbeddingMeta {
                        id: row.get(0)?,
                        asset_id: row.get(1)?,
                        purpose: row.get(2)?,
                        model_name: row.get(3)?,
                        model_revision: row.get(4)?,
                        dimensions: row.get::<_, i64>(5)?.max(0) as usize,
                        dtype: row.get(6)?,
                        source_hash: row.get(7)?,
                        created_at: row.get(8)?,
                    })
                },
            )
            .optional()
            .map_err(map_err)
    }

    /// モデルの全ベクトルを列挙する (総当たりcosine用。WP-12のUSearch
    /// 索引化の前の検証・acceptance経路)。
    pub fn embedding_vectors_for_model(
        &self,
        model_name: &str,
        model_revision: &str,
    ) -> Result<Vec<(i64, Vec<f32>)>, RepoError> {
        let mut stmt = self
            .conn()
            .prepare(
                "SELECT asset_id, vector FROM embeddings \
                 WHERE purpose = 'doc' AND model_name = ?1 AND model_revision = ?2 \
                 ORDER BY asset_id",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(params![model_name, model_revision], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
            })
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        rows.into_iter()
            .map(|(asset_id, blob)| decode_vector_f32(&blob).map(|v| (asset_id, v)))
            .collect()
    }

    /// モデルのベクトルをasset_id昇順にページ取得する (WP-12索引構築用 —
    /// 全件一括読み込みのメモリ峰值を避ける)。
    pub fn embedding_vectors_page(
        &self,
        model_name: &str,
        model_revision: &str,
        after_asset_id: i64,
        limit: u32,
    ) -> Result<Vec<(i64, Vec<f32>)>, RepoError> {
        let mut stmt = self
            .conn()
            .prepare(
                "SELECT asset_id, vector FROM embeddings \
                 WHERE purpose = 'doc' AND model_name = ?1 AND model_revision = ?2 \
                 AND asset_id > ?3 ORDER BY asset_id LIMIT ?4",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(
                params![model_name, model_revision, after_asset_id, limit],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        rows.into_iter()
            .map(|(asset_id, blob)| decode_vector_f32(&blob).map(|v| (asset_id, v)))
            .collect()
    }

    /// 1 assetのdocベクトルを取得する (類似画像検索のクエリ側)。
    pub fn embedding_vector_of_asset(
        &self,
        asset_id: i64,
        model_name: &str,
        model_revision: &str,
    ) -> Result<Option<Vec<f32>>, RepoError> {
        let blob: Option<Vec<u8>> = self
            .conn()
            .query_row(
                "SELECT vector FROM embeddings \
                 WHERE asset_id = ?1 AND purpose = 'doc' AND model_name = ?2 AND model_revision = ?3",
                params![asset_id, model_name, model_revision],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_err)?;
        blob.map(|bytes| decode_vector_f32(&bytes)).transpose()
    }

    /// モデルの既存ベクトルをすべて削除 (`embedding_model_delete`。
    /// モデル削除後も通常検索は動作する — acceptance)。
    pub fn embedding_delete_for_model(&self, model_name: &str) -> Result<u64, RepoError> {
        let changed = self
            .conn()
            .execute("DELETE FROM embeddings WHERE model_name = ?1", [model_name])
            .map_err(map_err)?;
        Ok(changed as u64)
    }

    /// 埋め込み済みasset件数 (モデル指定)。
    pub fn embedding_count_for_model(
        &self,
        model_name: &str,
        model_revision: &str,
    ) -> Result<u64, RepoError> {
        let count: i64 = self
            .conn()
            .query_row(
                "SELECT count(*) FROM embeddings \
                 WHERE purpose = 'doc' AND model_name = ?1 AND model_revision = ?2",
                params![model_name, model_revision],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        Ok(count.max(0) as u64)
    }

    /// 埋め込み対象となり得るasset件数 (presentなFileを持つasset)。
    pub fn embeddable_asset_count(&self) -> Result<u64, RepoError> {
        let count: i64 = self
            .conn()
            .query_row(
                "SELECT count(DISTINCT asset_id) FROM files \
                 WHERE scan_state = 'present' AND asset_id IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        Ok(count.max(0) as u64)
    }

    /// presentなFileを持つasset_idを昇順にページ取得 (埋め込み掃引用)。
    pub fn present_asset_ids(&self, after_id: i64, limit: u32) -> Result<Vec<i64>, RepoError> {
        let mut stmt = self
            .conn()
            .prepare(
                "SELECT DISTINCT asset_id FROM files \
                 WHERE scan_state = 'present' AND asset_id IS NOT NULL AND asset_id > ?1 \
                 ORDER BY asset_id LIMIT ?2",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(params![after_id, limit], |row| row.get::<_, i64>(0))
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    /// app_settings値の取得。
    pub fn setting_get(&self, key: &str) -> Result<Option<String>, RepoError> {
        self.conn()
            .query_row(
                "SELECT value FROM app_settings WHERE key = ?1",
                [key],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_err)
    }

    /// app_settings値の設定 (upsert)。
    pub fn setting_set(&self, key: &str, value: &str) -> Result<(), RepoError> {
        let now = now_utc_rfc3339();
        self.conn()
            .execute(
                "INSERT INTO app_settings (key, value, updated_at) VALUES (?1, ?2, ?3) \
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value, \
                 updated_at = excluded.updated_at",
                params![key, value, now],
            )
            .map_err(map_err)?;
        Ok(())
    }
}

/// 生Connection向けの設定取得 (トランザクション内呼び出し用の薄ラッパ)。
pub fn setting_get_raw(conn: &Connection, key: &str) -> Result<Option<String>, RepoError> {
    conn.query_row(
        "SELECT value FROM app_settings WHERE key = ?1",
        [key],
        |row| row.get(0),
    )
    .optional()
    .map_err(map_err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrate::{run_migrations, MigrationOpts};
    use crate::schema::MIGRATIONS;
    use rusqlite::Connection;

    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "gento-db-embed-{tag}-{}-{}",
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

    /// presentなFileを持つassetを作ってidを返す (rootは共用)。
    fn seed_present_asset(repos: &SqliteRepos<'_>, tag: &str) -> i64 {
        use gento_core::repo::{AssetRepo, FileRepo, LibraryRootRepo};
        let root = repos
            .list_roots()
            .unwrap()
            .into_iter()
            .next()
            .unwrap_or_else(|| {
                repos
                    .add_root(std::path::Path::new("/tmp/gento-embed-test"))
                    .unwrap()
            });
        let asset = repos
            .create_asset(gento_core::NewAsset {
                content_hash: format!("blake3:{tag}"),
                hash_algorithm: "blake3".to_string(),
                byte_size: 10,
                mime_type: None,
                width: None,
                height: None,
                has_alpha: None,
                is_animated: None,
                frame_count: None,
            })
            .unwrap();
        let file = repos
            .upsert_file(gento_core::FileUpsert {
                root_id: root.id,
                rel_path: std::path::PathBuf::from(format!("{tag}.png")),
                display_name: format!("{tag}.png"),
                byte_size: 10,
                mtime_ms: 1,
                mtime_ns: 1,
                scan_state: gento_core::ScanState::Present,
                os_file_id: None,
                detach_asset: false,
            })
            .unwrap();
        repos.link_file(file.id, asset.id).unwrap();
        asset.id
    }

    #[test]
    fn vector_encoding_round_trip() {
        let vector = [0.5f32, -1.25, 3.0, 0.0];
        let bytes = encode_vector_f32(&vector);
        assert_eq!(bytes.len(), 16);
        assert_eq!(&bytes[0..4], &0.5f32.to_le_bytes());
        let back = decode_vector_f32(&bytes).unwrap();
        assert_eq!(back, vector);
    }

    #[test]
    fn vector_decode_rejects_bad_length() {
        assert!(decode_vector_f32(&[1, 2, 3]).is_err());
        assert!(decode_vector_f32(&[]).is_ok());
    }

    #[test]
    fn upsert_and_read_back() {
        with_repos("upsert", |repos| {
            let asset_id = seed_present_asset(repos, "aa");
            let vector = vec![0.2f32; 8];
            repos
                .embedding_upsert(asset_id, "ruri-v3-70m", "rev1", 8, "f32", "hash-1", &vector)
                .expect("upsert");
            let meta = repos
                .embedding_meta_for(asset_id, "ruri-v3-70m", "rev1")
                .unwrap()
                .expect("meta");
            assert_eq!(meta.asset_id, asset_id);
            assert_eq!(meta.purpose, "doc");
            assert_eq!(meta.dimensions, 8);
            assert_eq!(meta.dtype, "f32");
            assert_eq!(meta.source_hash, "hash-1");
            let vectors = repos
                .embedding_vectors_for_model("ruri-v3-70m", "rev1")
                .unwrap();
            assert_eq!(vectors.len(), 1);
            assert_eq!(vectors[0].0, asset_id);
            assert_eq!(vectors[0].1.len(), 8);

            // 同一asset+modelのupsertは更新 (行は増えない)
            repos
                .embedding_upsert(asset_id, "ruri-v3-70m", "rev1", 8, "f32", "hash-2", &vector)
                .expect("upsert2");
            assert_eq!(
                repos
                    .embedding_count_for_model("ruri-v3-70m", "rev1")
                    .unwrap(),
                1
            );
            assert_eq!(
                repos
                    .embedding_meta_for(asset_id, "ruri-v3-70m", "rev1")
                    .unwrap()
                    .unwrap()
                    .source_hash,
                "hash-2"
            );
        });
    }

    #[test]
    fn delete_for_model_removes_vectors() {
        with_repos("delete", |repos| {
            let asset_id = seed_present_asset(repos, "bb");
            repos
                .embedding_upsert(asset_id, "ruri-v3-30m", "rev1", 4, "f32", "h", &[0.0; 4])
                .expect("upsert");
            let removed = repos
                .embedding_delete_for_model("ruri-v3-30m")
                .expect("delete");
            assert_eq!(removed, 1);
            assert_eq!(
                repos
                    .embedding_count_for_model("ruri-v3-30m", "rev1")
                    .unwrap(),
                0
            );
        });
    }

    #[test]
    fn present_asset_ids_paginate() {
        with_repos("paginate", |repos| {
            let a1 = seed_present_asset(repos, "c1");
            let a2 = seed_present_asset(repos, "c2");
            assert_eq!(repos.embeddable_asset_count().unwrap(), 2);
            let page1 = repos.present_asset_ids(0, 1).unwrap();
            assert_eq!(page1.len(), 1);
            assert_eq!(page1[0], a1.min(a2));
            let page2 = repos.present_asset_ids(page1[0], 5).unwrap();
            assert_eq!(page2, vec![a1.max(a2)]);
        });
    }

    #[test]
    fn app_settings_round_trip_and_conflict_update() {
        with_repos("settings", |repos| {
            repos.setting_set(SETTING_EMBEDDING_ENABLED, "1").unwrap();
            repos
                .setting_set(SETTING_EMBEDDING_MODEL, "ruri-v3-70m")
                .unwrap();
            assert_eq!(
                repos
                    .setting_get(SETTING_EMBEDDING_ENABLED)
                    .unwrap()
                    .as_deref(),
                Some("1")
            );
            assert_eq!(
                repos
                    .setting_get(SETTING_EMBEDDING_MODEL)
                    .unwrap()
                    .as_deref(),
                Some("ruri-v3-70m")
            );
            assert_eq!(repos.setting_get("no.such.key").unwrap(), None);
            repos.setting_set(SETTING_EMBEDDING_ENABLED, "0").unwrap();
            assert_eq!(
                repos
                    .setting_get(SETTING_EMBEDDING_ENABLED)
                    .unwrap()
                    .as_deref(),
                Some("0")
            );
        });
    }

    #[test]
    fn vectors_page_paginates_and_single_asset_fetch() {
        with_repos("page", |repos| {
            let a1 = seed_present_asset(repos, "p1");
            let a2 = seed_present_asset(repos, "p2");
            let a3 = seed_present_asset(repos, "p3");
            repos
                .embedding_upsert(a1, "m", "r1", 4, "f32", "h1", &[0.1; 4])
                .unwrap();
            repos
                .embedding_upsert(a2, "m", "r1", 4, "f32", "h2", &[0.2; 4])
                .unwrap();
            repos
                .embedding_upsert(a3, "m", "r2", 4, "f32", "h3", &[0.3; 4])
                .unwrap();
            // ページング (model+revision分離)
            let page1 = repos.embedding_vectors_page("m", "r1", 0, 2).unwrap();
            assert_eq!(page1.len(), 2);
            let last = page1.last().unwrap().0;
            let page2 = repos.embedding_vectors_page("m", "r1", last, 10).unwrap();
            assert_eq!(page2.len(), 0, "r1は2件のみ");
            let all = repos.embedding_vectors_page("m", "r2", 0, 10).unwrap();
            assert_eq!(all.len(), 1);
            assert_eq!(all[0].0, a3);
            // 1件取得 (類似検索クエリ側)
            assert_eq!(
                repos.embedding_vector_of_asset(a2, "m", "r1").unwrap(),
                Some(vec![0.2; 4])
            );
            assert_eq!(
                repos.embedding_vector_of_asset(a2, "m", "r2").unwrap(),
                None
            );
            assert_eq!(
                repos.embedding_vector_of_asset(999999, "m", "r1").unwrap(),
                None
            );
        });
    }

    #[test]
    fn embedding_job_type_is_accepted_by_jobs_check() {
        with_repos("jobtype", |repos| {
            let asset_id = seed_present_asset(repos, "dd");
            let job = repos
                .enqueue_job(JOB_TYPE_EMBEDDING, "asset", asset_id, 4, "{}")
                .expect("enqueue");
            assert!(job.is_some());
            assert_eq!(job.unwrap().job_type, "embedding");
        });
    }

    #[test]
    fn migration_v9_preserves_existing_jobs() {
        // #8のDB (embedding無しCHECK) から#9へ移行してもジョブが消えない
        let tmp = TempDir::new("migrate");
        let mut conn = Connection::open(tmp.0.join("db.sqlite3")).unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        run_migrations(&mut conn, &MIGRATIONS[..8], &MigrationOpts::default()).unwrap();
        conn.execute(
            "INSERT INTO jobs (uuid, job_type, target_type, target_id, status, priority, \
             attempts, max_attempts, payload, created_at, updated_at) \
             VALUES ('01923f1e-8ad2-7ccc-8000-000000000001', 'ai_caption_tags', 'asset', 1, \
             'succeeded', 5, 1, 3, '{}', '2026-09-02T00:00:00.000Z', '2026-09-02T00:00:00.000Z')",
            [],
        )
        .unwrap();
        run_migrations(&mut conn, MIGRATIONS, &MigrationOpts::default()).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 9);
        let (count, job_type): (i64, String) = conn
            .query_row("SELECT count(*), max(job_type) FROM jobs", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(job_type, "ai_caption_tags");
        // app_settingsが使える
        conn.execute(
            "INSERT INTO app_settings (key, value, updated_at) VALUES ('k', 'v', '2026-09-02')",
            [],
        )
        .unwrap();
    }
}
