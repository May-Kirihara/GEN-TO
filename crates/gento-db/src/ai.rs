//! AIプロファイル・実行履歴 (ai_profiles / ai_runs) のrepository実装
//! とAI成果物適用 (手動編集保護付き) (MVP WP-10)。
//!
//! - `ai_profiles` (data-model §8.1): `api_key_ref` は資格情報ストアの
//!   参照のみ。**鍵本体をDBへ書く経路は存在しない** (検証:
//!   gento-ai pipeline tests + api_keys_never_in_db)。
//! - `ai_runs` (§8.2): 試行ごとの履歴。「再試行の履歴を失わない」ため
//!   ジョブの再試行のたびに新規行を作る (UPDATEで使い回さない)。
//! - 適用規則 (spec §11.4「手動編集済み情報を保護する」):
//!   - タグ: 既存の同名タグ付与 (asset_tags行) は一切更新しない
//!     (ON CONFLICT DO NOTHING)。手動行・AI行を問わずスキップ。
//!   - キャプション: 現行行が `manually_edited=1` なら現行を保持し、
//!     AI結果は非現行 (current=0) のai行として履歴へ保存するのみ。
//!     それ以外は旧現行を降格して新現行ai行を挿入する。

use gento_core::metadata::{normalize_tag_name, CaptionKind};
use gento_core::repo::{FileRecord, LibraryRootRecord, RepoError};
use rusqlite::{params, Connection, OptionalExtension};

use crate::ids::{new_uuid_v7, now_utc_rfc3339};
use crate::repos::{map_err, SqliteRepos};

/// AI処理対象の解決結果 (assetの代表presentファイルと所属ルート)。
/// ルートの `send_exif` / `send_gps` は送信プライバシーの入力。
#[derive(Debug, Clone)]
pub struct AssetSource {
    pub root: LibraryRootRecord,
    pub file: FileRecord,
}

impl AssetSource {
    /// 送信対象ファイルの絶対パス。
    pub fn absolute_path(&self) -> std::path::PathBuf {
        self.root.path.join(&self.file.rel_path)
    }
}

/// ai_profiles行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiProfileRecord {
    pub id: i64,
    pub uuid: String,
    pub display_name: String,
    pub base_url: String,
    pub model: String,
    /// 資格情報ストア参照 ("keyring:..." / "file:...")。NULL = APIキー無し。
    pub api_key_ref: Option<String>,
    pub vision_capable: bool,
    pub enabled: bool,
    pub timeout_ms: i64,
    pub concurrency: i64,
    pub extra_json: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// 新規プロファイル投入値 (api_key_refは保存後に別途設定する)。
#[derive(Debug, Clone)]
pub struct NewAiProfile {
    pub display_name: String,
    pub base_url: String,
    pub model: String,
    pub vision_capable: bool,
    pub enabled: bool,
    pub timeout_ms: i64,
    pub concurrency: i64,
    pub extra_json: Option<String>,
}

/// ai_runs行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiRunRecord {
    pub id: i64,
    pub uuid: String,
    pub asset_id: i64,
    pub profile_id: Option<i64>,
    /// "caption" | "tags" | "caption_and_tags" | "embedding" | "rag"。
    pub operation: String,
    /// "running" | "succeeded" | "failed"。
    pub status: String,
    pub prompt_version: i64,
    pub request_summary: Option<String>,
    pub raw_response: Option<String>,
    pub error: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub created_at: String,
}

const PROFILE_COLUMNS: &str = "id, uuid, display_name, base_url, model, api_key_ref, \
     vision_capable, enabled, timeout_ms, concurrency, extra_json, created_at, updated_at";

const RUN_COLUMNS: &str = "id, uuid, asset_id, profile_id, operation, status, prompt_version, \
     request_summary, raw_response, error, started_at, finished_at, created_at";

fn map_profile(row: &rusqlite::Row<'_>) -> rusqlite::Result<AiProfileRecord> {
    Ok(AiProfileRecord {
        id: row.get(0)?,
        uuid: row.get(1)?,
        display_name: row.get(2)?,
        base_url: row.get(3)?,
        model: row.get(4)?,
        api_key_ref: row.get(5)?,
        vision_capable: row.get::<_, i64>(6)? != 0,
        enabled: row.get::<_, i64>(7)? != 0,
        timeout_ms: row.get(8)?,
        concurrency: row.get(9)?,
        extra_json: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

fn map_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<AiRunRecord> {
    Ok(AiRunRecord {
        id: row.get(0)?,
        uuid: row.get(1)?,
        asset_id: row.get(2)?,
        profile_id: row.get(3)?,
        operation: row.get(4)?,
        status: row.get(5)?,
        prompt_version: row.get(6)?,
        request_summary: row.get(7)?,
        raw_response: row.get(8)?,
        error: row.get(9)?,
        started_at: row.get(10)?,
        finished_at: row.get(11)?,
        created_at: row.get(12)?,
    })
}

fn get_profile_by_id(conn: &Connection, id: i64) -> Result<Option<AiProfileRecord>, RepoError> {
    let sql = format!("SELECT {PROFILE_COLUMNS} FROM ai_profiles WHERE id = ?1");
    conn.query_row(&sql, [id], map_profile)
        .optional()
        .map_err(map_err)
}

impl SqliteRepos<'_> {
    pub fn insert_ai_profile(&self, new: &NewAiProfile) -> Result<AiProfileRecord, RepoError> {
        let now = now_utc_rfc3339();
        self.conn()
            .execute(
                "INSERT INTO ai_profiles (uuid, display_name, base_url, model, \
                 vision_capable, enabled, timeout_ms, concurrency, extra_json, \
                 created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
                params![
                    new_uuid_v7(),
                    new.display_name,
                    new.base_url,
                    new.model,
                    new.vision_capable as i64,
                    new.enabled as i64,
                    new.timeout_ms,
                    new.concurrency,
                    new.extra_json,
                    now
                ],
            )
            .map_err(map_err)?;
        let id = self.conn().last_insert_rowid();
        get_profile_by_id(self.conn(), id)?.ok_or(RepoError::NotFound)
    }

    /// プロファイルの設定列を更新する (api_key_refは含まない —
    /// 鍵参照は [`SqliteRepos::set_ai_profile_api_key_ref`] で明示的に扱う)。
    pub fn update_ai_profile_settings(&self, record: &AiProfileRecord) -> Result<(), RepoError> {
        let now = now_utc_rfc3339();
        let changed = self
            .conn()
            .execute(
                "UPDATE ai_profiles SET display_name = ?2, base_url = ?3, model = ?4, \
                 vision_capable = ?5, enabled = ?6, timeout_ms = ?7, concurrency = ?8, \
                 extra_json = ?9, updated_at = ?10 WHERE id = ?1",
                params![
                    record.id,
                    record.display_name,
                    record.base_url,
                    record.model,
                    record.vision_capable as i64,
                    record.enabled as i64,
                    record.timeout_ms,
                    record.concurrency,
                    record.extra_json,
                    now
                ],
            )
            .map_err(map_err)?;
        if changed == 0 {
            return Err(RepoError::NotFound);
        }
        Ok(())
    }

    /// APIキー参照を設定する (Some=参照文字列 / None=クリア)。
    pub fn set_ai_profile_api_key_ref(
        &self,
        id: i64,
        api_key_ref: Option<&str>,
    ) -> Result<(), RepoError> {
        let now = now_utc_rfc3339();
        let changed = self
            .conn()
            .execute(
                "UPDATE ai_profiles SET api_key_ref = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, api_key_ref, now],
            )
            .map_err(map_err)?;
        if changed == 0 {
            return Err(RepoError::NotFound);
        }
        Ok(())
    }

    pub fn delete_ai_profile(&self, id: i64) -> Result<AiProfileRecord, RepoError> {
        let record = get_profile_by_id(self.conn(), id)?.ok_or(RepoError::NotFound)?;
        self.conn()
            .execute("DELETE FROM ai_profiles WHERE id = ?1", [id])
            .map_err(map_err)?;
        Ok(record)
    }

    pub fn list_ai_profiles(&self) -> Result<Vec<AiProfileRecord>, RepoError> {
        let sql = format!("SELECT {PROFILE_COLUMNS} FROM ai_profiles ORDER BY id ASC");
        let mut stmt = self.conn().prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map([], map_profile)
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    pub fn get_ai_profile(&self, id: i64) -> Result<Option<AiProfileRecord>, RepoError> {
        get_profile_by_id(self.conn(), id)
    }

    /// 自動処理で使う「有効プロファイル」(enabled=1のうち最新更新)。
    pub fn first_enabled_ai_profile(&self) -> Result<Option<AiProfileRecord>, RepoError> {
        let sql = format!(
            "SELECT {PROFILE_COLUMNS} FROM ai_profiles WHERE enabled = 1 \
             ORDER BY updated_at DESC, id ASC LIMIT 1"
        );
        self.conn()
            .query_row(&sql, [], map_profile)
            .optional()
            .map_err(map_err)
    }

    /// AI実行履歴行を開始 (status=running)。
    pub fn insert_ai_run(
        &self,
        asset_id: i64,
        profile_id: Option<i64>,
        operation: &str,
        prompt_version: i64,
        request_summary: Option<&str>,
    ) -> Result<AiRunRecord, RepoError> {
        let now = now_utc_rfc3339();
        self.conn()
            .execute(
                "INSERT INTO ai_runs (uuid, asset_id, profile_id, operation, status, \
                 prompt_version, request_summary, started_at, created_at) \
                 VALUES (?1, ?2, ?3, ?4, 'running', ?5, ?6, ?7, ?7)",
                params![
                    new_uuid_v7(),
                    asset_id,
                    profile_id,
                    operation,
                    prompt_version,
                    request_summary,
                    now
                ],
            )
            .map_err(map_err)?;
        let id = self.conn().last_insert_rowid();
        let sql = format!("SELECT {RUN_COLUMNS} FROM ai_runs WHERE id = ?1");
        self.conn()
            .query_row(&sql, [id], map_run)
            .optional()
            .map_err(map_err)?
            .ok_or(RepoError::NotFound)
    }

    /// AI実行履歴を確定させる (成功: raw_response / 失敗: error)。
    pub fn finish_ai_run(
        &self,
        id: i64,
        status: &str,
        raw_response: Option<&str>,
        error: Option<&str>,
    ) -> Result<(), RepoError> {
        let now = now_utc_rfc3339();
        let changed = self
            .conn()
            .execute(
                "UPDATE ai_runs SET status = ?2, raw_response = ?3, error = ?4, \
                 finished_at = ?5 WHERE id = ?1 AND status = 'running'",
                params![id, status, raw_response, error, now],
            )
            .map_err(map_err)?;
        if changed == 0 {
            return Err(RepoError::NotFound);
        }
        Ok(())
    }

    pub fn list_ai_runs_for_asset(
        &self,
        asset_id: i64,
        limit: u32,
    ) -> Result<Vec<AiRunRecord>, RepoError> {
        let sql = format!(
            "SELECT {RUN_COLUMNS} FROM ai_runs WHERE asset_id = ?1 \
             ORDER BY id DESC LIMIT ?2"
        );
        let mut stmt = self.conn().prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(params![asset_id, limit], map_run)
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    /// 当該assetが指定operation+prompt_versionで成功済みか
    /// (自動エンキューの重複抑止。プロファイル切替では再実行しない —
    /// 再生成は明示操作。spec §11.3/§11.4)。
    pub fn has_succeeded_ai_run(
        &self,
        asset_id: i64,
        operation: &str,
        prompt_version: i64,
    ) -> Result<bool, RepoError> {
        let hit: i64 = self
            .conn()
            .query_row(
                "SELECT count(*) FROM ai_runs WHERE asset_id = ?1 AND operation = ?2 \
                 AND prompt_version = ?3 AND status = 'succeeded'",
                params![asset_id, operation, prompt_version],
                |row| row.get(0),
            )
            .map_err(map_err)?;
        Ok(hit > 0)
    }

    // ===== AI成果物の適用 (手動編集保護) =====

    /// assetの代表presentファイル (id最小) と所属ルートを解決する。
    /// presentファイルが無い (全ファイルmissing等) 場合はNone。
    pub fn representative_present_file(
        &self,
        asset_id: i64,
    ) -> Result<Option<AssetSource>, RepoError> {
        let file = {
            let sql = format!(
                "SELECT {} FROM files \
                 WHERE asset_id = ?1 AND scan_state = 'present' ORDER BY id ASC LIMIT 1",
                crate::repos::FILE_COLUMNS
            );
            let mut stmt = self.conn().prepare(&sql).map_err(map_err)?;
            stmt.query_row([asset_id], crate::repos::map_file)
                .optional()
                .map_err(map_err)?
        };
        let Some(file) = file else {
            return Ok(None);
        };
        let root = self.get_root(file.root_id)?.ok_or(RepoError::NotFound)?;
        Ok(Some(AssetSource { root, file }))
    }

    /// AIタグを付与する。既存の同名付与 (手動・AI問わず) はスキップ。
    /// 戻り値 = 新規に付与した件数。
    pub fn add_ai_tags(
        &self,
        asset_id: i64,
        names: &[String],
        ai_run_id: i64,
    ) -> Result<u64, RepoError> {
        if names.is_empty() {
            return Ok(0);
        }
        let tx = self
            .conn()
            .unchecked_transaction()
            .map_err(|e| RepoError::Db(e.to_string()))?;
        let now = now_utc_rfc3339();
        let mut applied: u64 = 0;
        for name in names {
            let normalized = normalize_tag_name(name);
            if normalized.is_empty() {
                continue;
            }
            let tag_id: Option<i64> = tx
                .query_row(
                    "SELECT id FROM tags WHERE normalized_name = ?1",
                    [&normalized],
                    |row| row.get(0),
                )
                .optional()
                .map_err(map_err)?;
            let tag_id = match tag_id {
                Some(id) => id,
                None => {
                    tx.execute(
                        "INSERT INTO tags (uuid, display_name, normalized_name, \
                         created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
                        params![new_uuid_v7(), name.trim(), normalized, now],
                    )
                    .map_err(map_err)?;
                    tx.last_insert_rowid()
                }
            };
            // 既存付与 (manually_edited=1を含む) は絶対に更新しない。
            let changed = tx
                .execute(
                    "INSERT INTO asset_tags (asset_id, tag_id, source, ai_confidence, \
                     manually_edited, ai_run_id, created_at) \
                     VALUES (?1, ?2, 'ai', NULL, 0, ?3, ?4) \
                     ON CONFLICT (asset_id, tag_id) DO NOTHING",
                    params![asset_id, tag_id, ai_run_id, now],
                )
                .map_err(map_err)?;
            applied += changed as u64;
        }
        crate::search::rebuild_docs_for_assets(&tx, &[asset_id])?;
        tx.commit().map_err(map_err)?;
        Ok(applied)
    }

    /// AIキャプションを適用する。現行行が手動編集済みなら現行を保持し、
    /// AI結果は非現行ai行として保存するのみ (戻り値false)。
    pub fn apply_ai_caption(
        &self,
        asset_id: i64,
        kind: CaptionKind,
        body: &str,
        ai_run_id: i64,
    ) -> Result<bool, RepoError> {
        let tx = self
            .conn()
            .unchecked_transaction()
            .map_err(|e| RepoError::Db(e.to_string()))?;
        let now = now_utc_rfc3339();
        let current_manually_edited: Option<i64> = tx
            .query_row(
                "SELECT manually_edited FROM captions \
                 WHERE asset_id = ?1 AND kind = ?2 AND current = 1",
                params![asset_id, kind.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_err)?;
        let becomes_current = current_manually_edited != Some(1);
        if becomes_current {
            tx.execute(
                "UPDATE captions SET current = 0, updated_at = ?3 \
                 WHERE asset_id = ?1 AND kind = ?2 AND current = 1",
                params![asset_id, kind.as_str(), now],
            )
            .map_err(map_err)?;
        }
        tx.execute(
            "INSERT INTO captions (uuid, asset_id, kind, body, source, current, \
             manually_edited, ai_run_id, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, 'ai', ?5, 0, ?6, ?7, ?7)",
            params![
                new_uuid_v7(),
                asset_id,
                kind.as_str(),
                body,
                becomes_current as i64,
                ai_run_id,
                now
            ],
        )
        .map_err(map_err)?;
        crate::search::rebuild_docs_for_assets(&tx, &[asset_id])?;
        tx.commit().map_err(map_err)?;
        Ok(becomes_current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrate::{run_migrations, MigrationOpts};
    use crate::schema::MIGRATIONS;
    use gento_core::metadata::{CaptionRepo, CaptionSource};
    use gento_core::repo::{AssetRepo, NewAsset};
    use std::path::PathBuf;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "gento-db-ai-{tag}-{}-{}",
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

    fn asset(repos: &SqliteRepos<'_>, hash: &str) -> i64 {
        repos
            .create_asset(NewAsset {
                content_hash: hash.to_string(),
                hash_algorithm: "blake3".to_string(),
                byte_size: 1,
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

    fn profile(repos: &SqliteRepos<'_>, name: &str) -> AiProfileRecord {
        repos
            .insert_ai_profile(&NewAiProfile {
                display_name: name.to_string(),
                base_url: "http://127.0.0.1:1/v1".to_string(),
                model: "test-model".to_string(),
                vision_capable: true,
                enabled: true,
                timeout_ms: 30_000,
                concurrency: 2,
                extra_json: None,
            })
            .unwrap()
    }

    fn run(repos: &SqliteRepos<'_>, asset_id: i64, profile_id: i64) -> AiRunRecord {
        repos
            .insert_ai_run(asset_id, Some(profile_id), "caption_and_tags", 1, None)
            .unwrap()
    }

    #[test]
    fn profile_crud_roundtrip() {
        with_repos("crud", |repos| {
            let created = profile(repos, "ローカルllama");
            assert!(created.enabled);
            assert!(created.api_key_ref.is_none());
            // 参照の設定・更新
            repos
                .set_ai_profile_api_key_ref(created.id, Some("keyring:gento/x"))
                .unwrap();
            let loaded = repos.get_ai_profile(created.id).unwrap().unwrap();
            assert_eq!(loaded.api_key_ref.as_deref(), Some("keyring:gento/x"));
            let mut updated = loaded.clone();
            updated.enabled = false;
            repos.update_ai_profile_settings(&updated).unwrap();
            assert!(repos.first_enabled_ai_profile().unwrap().is_none());
            // 削除
            let deleted = repos.delete_ai_profile(created.id).unwrap();
            assert_eq!(deleted.id, created.id);
            assert!(repos.list_ai_profiles().unwrap().is_empty());
        });
    }

    #[test]
    fn delete_profile_keeps_runs_with_null_profile() {
        with_repos("delprofile", |repos| {
            let p = profile(repos, "p");
            let a = asset(repos, "blake3:x");
            let r = run(repos, a, p.id);
            repos.delete_ai_profile(p.id).unwrap();
            let runs = repos.list_ai_runs_for_asset(a, 10).unwrap();
            assert_eq!(runs.len(), 1, "履歴は消えない");
            assert_eq!(runs[0].profile_id, None, "ON DELETE SET NULL");
            assert_eq!(runs[0].uuid, r.uuid);
        });
    }

    #[test]
    fn ai_tags_skip_existing_and_never_touch_manual_rows() {
        with_repos("aitags", |repos| {
            use gento_core::metadata::AssetTagRepo;
            let a = asset(repos, "blake3:t1");
            let p = profile(repos, "p");
            let r = run(repos, a, p.id);
            // 手動で「空」タグを付与 (manually_edited=1)
            repos.add_tags_batch(&[a], &["空"]).unwrap();
            let applied = repos
                .add_ai_tags(a, &["空".to_string(), "clouds".to_string()], r.id)
                .unwrap();
            assert_eq!(applied, 1, "既存の「空」はスキップされcloudsのみ追加");
            let tags = repos.list_asset_tags(a).unwrap();
            let empty = tags.iter().find(|t| t.tag.display_name == "空").unwrap();
            assert_eq!(empty.source.as_str(), "manual", "手動行のsourceは不変");
            assert!(empty.manually_edited);
            assert!(empty.tag.id > 0);
            let clouds = tags
                .iter()
                .find(|t| t.tag.display_name == "clouds")
                .unwrap();
            assert_eq!(clouds.source.as_str(), "ai");
            // 2回目の適用は全スキップ
            let again = repos.add_ai_tags(a, &["clouds".to_string()], r.id).unwrap();
            assert_eq!(again, 0);
        });
    }

    #[test]
    fn ai_caption_replaces_ai_current_but_preserves_manual_current() {
        with_repos("aicaption", |repos| {
            let a = asset(repos, "blake3:c1");
            let p = profile(repos, "p");
            let r1 = run(repos, a, p.id);
            // ① AI現行を挿入
            assert!(repos
                .apply_ai_caption(a, CaptionKind::Description, "AI説明v1", r1.id)
                .unwrap());
            let c1 = repos
                .get_current_caption(a, CaptionKind::Description)
                .unwrap()
                .unwrap();
            assert_eq!(c1.body, "AI説明v1");
            assert_eq!(c1.source, CaptionSource::Ai);
            // ② 再生成: AI現行を差し替え (旧は履歴へ)
            let r2 = run(repos, a, p.id);
            assert!(repos
                .apply_ai_caption(a, CaptionKind::Description, "AI説明v2", r2.id)
                .unwrap());
            let history = repos
                .list_caption_history(a, CaptionKind::Description, 10)
                .unwrap();
            assert_eq!(history.len(), 2);
            let c2 = repos
                .get_current_caption(a, CaptionKind::Description)
                .unwrap()
                .unwrap();
            assert_eq!(c2.body, "AI説明v2");
            assert!(!c2.manually_edited);
            // ③ 手動編集後の再生成: 現行 (手動) は絶対保持
            repos
                .set_caption(a, CaptionKind::Description, "手動で直した")
                .unwrap();
            let r3 = run(repos, a, p.id);
            assert!(
                !repos
                    .apply_ai_caption(a, CaptionKind::Description, "AI説明v3", r3.id)
                    .unwrap(),
                "手動現行のため非現行保存"
            );
            let manual = repos
                .get_current_caption(a, CaptionKind::Description)
                .unwrap()
                .unwrap();
            assert_eq!(manual.body, "手動で直した");
            assert_eq!(manual.source, CaptionSource::Manual);
            assert!(manual.manually_edited);
            // AI v3は履歴に非現行で存在
            let history = repos
                .list_caption_history(a, CaptionKind::Description, 10)
                .unwrap();
            assert_eq!(history.len(), 4, "v1, v2, 手動, v3(非現行)");
            let v3 = history.iter().find(|c| c.body == "AI説明v3").unwrap();
            assert!(!v3.is_current);
            assert_eq!(v3.source, CaptionSource::Ai);
        });
    }

    #[test]
    fn delete_asset_cascades_ai_rows_and_tags() {
        with_repos("delasset", |repos| {
            let a = asset(repos, "blake3:d1");
            let p = profile(repos, "p");
            let r = run(repos, a, p.id);
            repos.add_ai_tags(a, &["x".to_string()], r.id).unwrap();
            repos
                .apply_ai_caption(a, CaptionKind::Description, "cap", r.id)
                .unwrap();
            // CASCADE順序実証: ai_runs CASCADE と asset_tags(CASCADE + ai_run_id FK)
            // が競合してもasset削除は完結する (repo APIにasset削除は無いため
            // 生SQLで実行 — files.asset_idはRESTRICTだがFileを作っていない)
            repos
                .conn()
                .execute("DELETE FROM assets WHERE id = ?1", [a])
                .unwrap();
            let runs: i64 = repos
                .conn()
                .query_row("SELECT count(*) FROM ai_runs", [], |row| row.get(0))
                .unwrap();
            assert_eq!(runs, 0);
            let tags: i64 = repos
                .conn()
                .query_row("SELECT count(*) FROM asset_tags", [], |row| row.get(0))
                .unwrap();
            assert_eq!(tags, 0);
        });
    }

    #[test]
    fn has_succeeded_ai_run_guards_by_prompt_version() {
        with_repos("guard", |repos| {
            let a = asset(repos, "blake3:g1");
            let p = profile(repos, "p");
            assert!(!repos
                .has_succeeded_ai_run(a, "caption_and_tags", 1)
                .unwrap());
            let r = run(repos, a, p.id);
            repos
                .finish_ai_run(r.id, "failed", None, Some("endpoint down"))
                .unwrap();
            assert!(!repos
                .has_succeeded_ai_run(a, "caption_and_tags", 1)
                .unwrap());
            let r2 = run(repos, a, p.id);
            repos
                .finish_ai_run(r2.id, "succeeded", Some("{}"), None)
                .unwrap();
            assert!(repos
                .has_succeeded_ai_run(a, "caption_and_tags", 1)
                .unwrap());
            assert!(!repos
                .has_succeeded_ai_run(a, "caption_and_tags", 2)
                .unwrap());
        });
    }
}
