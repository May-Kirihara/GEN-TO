//! USearch索引の管理サービス (MVP WP-12)。
//!
//! - モデルidentity (name+revision+dims) ごとに `data_dir/indexes/{model}/`
//!   へ索引を保存し、起動・有効化時に再読込する (acceptance)。
//! - 破損・欠落・SQLiteとの件数不整合はバックグラウンド全再構築で復旧
//!   (進捗イベント `vector-index:progress`。UI非ブロック)。
//! - 埋め込みジョブ完了のフックで増分追加 (`on_embedding_upserted`)。
//!   ディスク反映は定期保存 (30秒毎・構築中でないとき)。
//! - モデル切り替え中は旧索引を維持し、新モデルの構築完了後に切替
//!   (§10のアトミック切替 — 世代はidentity照合で判定)。
//!
//! ベクトル正本はSQLite embeddings表。索引はあくまで派生データであり、
//! いつでも破棄して再構築できる。

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gento_db::Db;
use serde::Serialize;
use tauri::State;

use gento_embedding::{IndexIdentity, Neighbor, VectorIndex};

use crate::catalog::AppState;

/// 通知・進捗イベント送出境界 (Tauri emit / テスト記録)。payloadはJSON文字列。
type NoticeSink = Arc<dyn Fn(&str, &str) + Send + Sync>;

/// 索引構築の1ページ (SQLite読み出し件数)。
const BUILD_PAGE: u32 = 1024;
/// 進捗イベントの送出間隔 (ページ数)。
const PROGRESS_EVERY_PAGES: u32 = 4;
/// 増分追加のディスク反映を促す間隔。
const SAVE_SWEEP_INTERVAL: Duration = Duration::from_secs(30);
/// メンテナンスループの待機間隔。
const MAINTENANCE_POLL: Duration = Duration::from_millis(500);

// ===== DTO =====

/// `vector_index_status` 応答DTO。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VectorIndexStatusDto {
    /// 現在の選択モデル (索引の対象)。
    pub model: String,
    /// 検索可能状態 (索引ロード済みかつidentity一致)。
    pub ready: bool,
    /// 索引内ベクトル数。
    pub indexed_count: u64,
    /// SQLite embeddings表のベクトル数 (正本)。
    pub sqlite_count: u64,
    /// バックグラウンド構築中。
    pub building: bool,
    /// 直近エラー ( ロード失敗・構築失敗等)。
    pub last_error: Option<String>,
}

/// `vector-index:progress` イベントペイロード。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct ProgressPayload {
    done: u64,
    total: u64,
}

/// `vector-index:ready` イベントペイロード。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct ReadyPayload {
    model: String,
    count: u64,
}

// ===== サービス =====

/// 稼働中の索引スロット (identity照合でモデル切替を検出)。
#[derive(Default)]
struct IndexSlot {
    index: Option<Arc<VectorIndex>>,
    identity: Option<IndexIdentity>,
    last_error: Option<String>,
}

pub struct VectorIndexService {
    db: Arc<Db>,
    indexes_root: PathBuf,
    slot: Mutex<IndexSlot>,
    building: AtomicBool,
    shutdown: AtomicBool,
    /// 増分追加のうち未保存の件数。
    dirty: AtomicU64,
    /// メンテナンスループでensureを試みた済みのモデル (再起動直後1回)。
    ensured_for: Mutex<Option<String>>,
    emit: NoticeSink,
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

impl VectorIndexService {
    pub fn start(db: Arc<Db>, data_dir: PathBuf, emit: NoticeSink) -> Arc<Self> {
        let service = Arc::new(Self {
            db,
            indexes_root: data_dir.join("indexes"),
            slot: Mutex::new(IndexSlot::default()),
            building: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            dirty: AtomicU64::new(0),
            ensured_for: Mutex::new(None),
            emit,
            threads: Mutex::new(Vec::new()),
        });
        let service2 = Arc::clone(&service);
        let handle = std::thread::Builder::new()
            .name("vector-index-maint".into())
            .spawn(move || maintenance_loop(&service2))
            .expect("spawn vector index maintenance");
        service
            .threads
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(handle);
        service
    }

    /// 現在の選択モデルとそのidentity (manifest由来)。
    /// 設定未設定時は既定モデル (ruri-v3-70m) へフォールバック —
    /// EmbeddingServiceと同一規則。
    fn current_identity(&self) -> Option<IndexIdentity> {
        let model = crate::embedding::selected_model_of(&self.db);
        identity_of_model(&model)
    }

    /// モデルの索引ディレクトリ (`indexes/{model}`)。
    fn model_dir(&self, model: &str) -> PathBuf {
        self.indexes_root.join(model)
    }

    /// 索引が検索可能か (ロード済み・identity一致)。
    pub fn is_ready(&self) -> bool {
        let Some(current) = self.current_identity() else {
            return false;
        };
        let slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
        slot.identity
            .as_ref()
            .is_some_and(|identity| *identity == current)
            && slot.index.is_some()
    }

    /// 近傍検索 (検索可能でない場合はNone)。
    pub fn search(
        &self,
        query: &[f32],
        k: usize,
        allowed: Option<&HashSet<i64>>,
    ) -> Option<Vec<Neighbor>> {
        let current = self.current_identity()?;
        let mut slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
        let ready = slot
            .identity
            .as_ref()
            .is_some_and(|identity| *identity == current);
        if !ready {
            return None;
        }
        let index = slot.index.as_ref()?;
        let result = match allowed {
            Some(allowed) => index.filtered_search(query, k, allowed),
            None => index.search(query, k),
        };
        match result {
            Ok(neighbors) => Some(neighbors),
            Err(error) => {
                slot.last_error = Some(error.to_string());
                tracing::warn!(error = %error, "vector index search failed");
                None
            }
        }
    }

    /// 同期ensure: ファイルがあれば再読込、なければバックグラウンド再構築
    /// を予約する。起動時・有効化時に呼ぶ (構築自体はUIをブロックしない)。
    pub fn ensure_ready(self: &Arc<Self>) {
        let Some(current) = self.current_identity() else {
            return;
        };
        let sqlite_count = self.sqlite_count_of(&current);
        {
            let slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
            let already = slot
                .identity
                .as_ref()
                .is_some_and(|identity| *identity == current)
                && slot
                    .index
                    .as_ref()
                    .is_some_and(|index| index.len() as u64 == sqlite_count);
            if already {
                return;
            }
        }
        let dir = self.model_dir(&current.model_name);
        if gento_embedding::index_files_exist(&dir) {
            match VectorIndex::open(&dir, &current) {
                Ok(index) => {
                    let loaded_len = index.len() as u64;
                    let mut slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
                    slot.identity = Some(current.clone());
                    slot.index = Some(Arc::new(index));
                    slot.last_error = None;
                    tracing::info!(
                        model = %current.model_name,
                        loaded = loaded_len,
                        sqlite = sqlite_count,
                        "vector index loaded"
                    );
                    self.emit_ready(&current.model_name, loaded_len);
                    if loaded_len != sqlite_count {
                        // 件数不整合: 旧索引で検索可能にしつつ背景で全再構築
                        self.schedule_rebuild();
                    }
                    return;
                }
                Err(error) => {
                    // 破損・形式不一致: 破棄して再構築 (acceptance: 索引削除
                    // →SQLiteから再構築)
                    tracing::warn!(
                        error = %error,
                        model = %current.model_name,
                        "vector index unusable; discarding and rebuilding"
                    );
                    self.slot
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .last_error = Some(error.to_string());
                    let _ = gento_embedding::remove_index_files(&dir);
                }
            }
        }
        self.schedule_rebuild();
    }

    /// バックグラウンド全再構築を予約 (二重起動防止付き)。
    pub fn schedule_rebuild(self: &Arc<Self>) {
        if self.shutdown.load(Ordering::SeqCst) {
            return;
        }
        if self.building.swap(true, Ordering::SeqCst) {
            return;
        }
        let service = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("vector-index-build".into())
            .spawn(move || {
                build_index(&service);
                service.building.store(false, Ordering::SeqCst);
            });
        match spawned {
            Ok(handle) => {
                self.threads
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(handle);
            }
            Err(error) => {
                tracing::warn!(error = %error, "vector index build spawn failed");
                self.building.store(false, Ordering::SeqCst);
            }
        }
    }

    /// 手動再構築コマンドの本体: 稼働索引を外し、ファイルも破棄して
    /// バックグラウンド再構築へ (即時復帰)。
    pub fn rebuild_now(self: &Arc<Self>) {
        if let Some(current) = self.current_identity() {
            let dir = self.model_dir(&current.model_name);
            let _ = gento_embedding::remove_index_files(&dir);
        }
        {
            let mut slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
            slot.index = None;
            slot.identity = None;
        }
        self.schedule_rebuild();
    }

    /// モデル削除に追随: 該当モデルの索引ファイルとスロットを破棄。
    pub fn delete_index_for_model(&self, model: &str) {
        let dir = self.model_dir(model);
        let _ = gento_embedding::remove_index_files(&dir);
        let mut slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
        if slot
            .identity
            .as_ref()
            .is_some_and(|identity| identity.model_name == model)
        {
            slot.index = None;
            slot.identity = None;
        }
    }

    /// 埋め込みジョブ完了フック (EmbeddingServiceから)。索引が稼働中なら
    /// 増分追加する。
    pub fn on_embedding_upserted(
        &self,
        model: &str,
        revision: &str,
        asset_id: i64,
        vector: &[f32],
    ) {
        let Some(current) = self.current_identity() else {
            return;
        };
        if current.model_name != model || current.model_revision != revision {
            return;
        }
        let mut slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
        let ready = slot
            .identity
            .as_ref()
            .is_some_and(|identity| *identity == current);
        if !ready {
            return;
        }
        if let Some(index) = slot.index.as_ref() {
            if index.contains(asset_id) {
                return;
            }
            if let Err(error) = index.add(asset_id, vector) {
                tracing::warn!(error = %error, "vector index incremental add failed");
                slot.last_error = Some(error.to_string());
                return;
            }
            self.dirty.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// 増分追加をディスクへ反映 (索引ロード済み・構築中でないとき)。
    /// 公開版: graceful shutdown時の保存とテストからも使う。
    pub fn flush(&self) {
        self.save_if_dirty();
    }

    fn save_if_dirty(&self) {
        let dirty = self.dirty.swap(0, Ordering::SeqCst);
        if dirty == 0 || self.building.load(Ordering::SeqCst) {
            if dirty > 0 {
                // 構築中は見送った分を戻しておく (構築完了saveが全体を書く)
                self.dirty.fetch_add(dirty, Ordering::SeqCst);
            }
            return;
        }
        let Some(current) = self.current_identity() else {
            return;
        };
        let mut slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
        let ready = slot
            .identity
            .as_ref()
            .is_some_and(|identity| *identity == current);
        if !ready {
            return;
        }
        if let Some(index) = slot.index.as_ref() {
            let dir = self.model_dir(&current.model_name);
            if let Err(error) = index.save(&dir) {
                tracing::warn!(error = %error, "vector index periodic save failed");
                slot.last_error = Some(error.to_string());
            } else {
                tracing::debug!(count = index.len(), "vector index saved");
            }
        }
    }

    fn sqlite_count_of(&self, identity: &IndexIdentity) -> u64 {
        self.db
            .with_reader(|repos| {
                repos.embedding_count_for_model(&identity.model_name, &identity.model_revision)
            })
            .unwrap_or(0)
    }

    fn emit_progress(&self, done: u64, total: u64) {
        if let Ok(payload) = serde_json::to_string(&ProgressPayload { done, total }) {
            (self.emit)("vector-index:progress", &payload);
        }
    }

    fn emit_ready(&self, model: &str, count: u64) {
        if let Ok(payload) = serde_json::to_string(&ReadyPayload {
            model: model.to_string(),
            count,
        }) {
            (self.emit)("vector-index:ready", &payload);
        }
    }

    /// 状態スナップショット。
    pub fn status(&self) -> VectorIndexStatusDto {
        let current = self.current_identity();
        let model = current
            .as_ref()
            .map(|identity| identity.model_name.clone())
            .unwrap_or_default();
        let sqlite_count = current
            .as_ref()
            .map(|identity| self.sqlite_count_of(identity))
            .unwrap_or(0);
        let (ready, indexed_count, last_error) = {
            let slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
            let ready = current
                .as_ref()
                .is_some_and(|current| slot.identity.as_ref() == Some(current))
                && slot.index.is_some();
            let indexed_count = slot.index.as_ref().map_or(0, |index| index.len() as u64);
            (ready, indexed_count, slot.last_error.clone())
        };
        VectorIndexStatusDto {
            model,
            ready,
            indexed_count,
            sqlite_count,
            building: self.building.load(Ordering::SeqCst),
            last_error,
        }
    }

    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
}

impl Drop for VectorIndexService {
    fn drop(&mut self) {
        self.shutdown();
        // 終了時に増分を保存 (crash時は次回起動の件数照合→再構築が拾う)
        self.flush();
        for handle in self
            .threads
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .drain(..)
        {
            let _ = handle.join();
        }
    }
}

/// モデルID → identity (manifestが正本。未知モデルはNone)。
pub fn identity_of_model(model: &str) -> Option<IndexIdentity> {
    let spec = gento_embedding::manifest::model_spec(model).ok()?;
    Some(IndexIdentity {
        model_name: model.to_string(),
        model_revision: spec.revision,
        dimensions: spec.embedding_dim,
    })
}

/// バックグラウンド構築スレッド本体: SQLiteからページ読み出し→追加→
/// 保存→スロット切替 (§10: 完了後にアトミックに切り替える)。
fn build_index(service: &VectorIndexService) {
    let Some(identity) = service.current_identity() else {
        return;
    };
    let total = service.sqlite_count_of(&identity);
    let dir = service.model_dir(&identity.model_name);
    let index = match VectorIndex::new(identity.clone(), total as usize) {
        Ok(index) => index,
        Err(error) => {
            record_build_error(service, &identity, error.to_string());
            return;
        }
    };
    let mut added: u64 = 0;
    let mut after_id: i64 = 0;
    let mut pages: u32 = 0;
    loop {
        let page: Vec<(i64, Vec<f32>)> = match service.db.with_reader(|repos| {
            repos.embedding_vectors_page(
                &identity.model_name,
                &identity.model_revision,
                after_id,
                BUILD_PAGE,
            )
        }) {
            Ok(page) => page,
            Err(error) => {
                record_build_error(service, &identity, format!("SQLite読み出し失敗: {error}"));
                return;
            }
        };
        if page.is_empty() {
            break;
        }
        after_id = page.last().map_or(0, |(id, _)| *id);
        for (asset_id, vector) in &page {
            if let Err(error) = index.add(*asset_id, vector) {
                record_build_error(service, &identity, error.to_string());
                return;
            }
        }
        added += page.len() as u64;
        pages += 1;
        if pages.is_multiple_of(PROGRESS_EVERY_PAGES) || added >= total {
            service.emit_progress(added.min(total), total);
        }
        if service.shutdown.load(Ordering::SeqCst) {
            tracing::info!("vector index build interrupted by shutdown");
            return;
        }
    }
    if let Err(error) = index.save(&dir) {
        record_build_error(service, &identity, error.to_string());
        return;
    }
    // 切替 (構築中にモデルが変わっていたら破棄)
    let still_current = service
        .current_identity()
        .is_some_and(|current| current == identity);
    if still_current {
        let count = index.len() as u64;
        {
            let mut slot = service.slot.lock().unwrap_or_else(|p| p.into_inner());
            slot.identity = Some(identity.clone());
            slot.index = Some(Arc::new(index));
            slot.last_error = None;
        }
        service.dirty.store(0, Ordering::SeqCst);
        tracing::info!(model = %identity.model_name, count, "vector index built");
        service.emit_ready(&identity.model_name, count);
    } else {
        tracing::info!(
            model = %identity.model_name,
            "model switched during build; discarding result"
        );
    }
}

fn record_build_error(service: &VectorIndexService, identity: &IndexIdentity, message: String) {
    tracing::warn!(model = %identity.model_name, error = %message, "vector index build failed");
    service
        .slot
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .last_error = Some(message.clone());
    let payload = format!("索引の構築に失敗しました: {message}");
    (service.emit)("vector-index:notice", &payload);
}

/// メンテナンスループ: 起動時ensure・モデル切替検出・定期保存。
fn maintenance_loop(service: &Arc<VectorIndexService>) {
    let mut last_save_sweep = std::time::Instant::now();
    loop {
        if service.shutdown.load(Ordering::SeqCst) {
            break;
        }
        // 埋め込み有効時のみ仕事がある (有効判定はベクトル件数で代用 —
        // 設定無効でも残存ベクトルの索引は維持してよい)
        let current = service.current_identity();
        if let Some(identity) = &current {
            let mut ensured = service
                .ensured_for
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if ensured.as_deref() != Some(identity.model_name.as_str())
                && service.sqlite_count_of(identity) > 0
            {
                *ensured = Some(identity.model_name.clone());
                drop(ensured);
                service.ensure_ready();
            }
        }
        if last_save_sweep.elapsed() >= SAVE_SWEEP_INTERVAL {
            service.save_if_dirty();
            last_save_sweep = std::time::Instant::now();
        }
        std::thread::sleep(MAINTENANCE_POLL);
    }
}

/// Tauriイベント送出境界。
pub fn tauri_notice_sink(app: tauri::AppHandle) -> NoticeSink {
    Arc::new(move |event: &str, payload: &str| {
        use tauri::Emitter;
        let _ = app.emit(event, payload);
    })
}

// ===== Tauri command =====

fn service_of(state: &State<'_, AppState>) -> Result<Arc<VectorIndexService>, String> {
    state
        .vector_index
        .clone()
        .ok_or_else(|| "索引サービスが利用できません (DBが開けていません)".to_string())
}

/// 索引の状態 (検索バーの意味検索可用性表示に使用)。
#[tauri::command]
pub async fn vector_index_status(
    state: State<'_, AppState>,
) -> Result<VectorIndexStatusDto, String> {
    let service = service_of(&state)?;
    tauri::async_runtime::spawn_blocking(move || service.status())
        .await
        .map_err(|e| e.to_string())
}

/// 索引を全再構築する (バックグラウンド。進捗は
/// `vector-index:progress` イベント)。
#[tauri::command]
pub async fn vector_index_rebuild(
    state: State<'_, AppState>,
) -> Result<VectorIndexStatusDto, String> {
    let service = service_of(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        service.rebuild_now();
        service.status()
    })
    .await
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ===== DTO直列化形状 (FIX-AI-DTO教訓) =====

    #[test]
    fn status_dto_serializes_camel_case() {
        let dto = VectorIndexStatusDto {
            model: "ruri-v3-70m".to_string(),
            ready: true,
            indexed_count: 100,
            sqlite_count: 100,
            building: false,
            last_error: None,
        };
        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["model"], "ruri-v3-70m");
        assert_eq!(json["ready"], true);
        assert_eq!(json["indexedCount"], 100);
        assert_eq!(json["sqliteCount"], 100);
        assert_eq!(json["building"], false);
        assert_eq!(json["lastError"], serde_json::Value::Null);
        for banned in ["indexed_count", "sqlite_count", "last_error"] {
            assert!(json.get(banned).is_none(), "snake_case {banned} が混入");
        }
    }

    #[test]
    fn progress_and_ready_payloads_are_camel_case() {
        let json = serde_json::to_value(ProgressPayload { done: 3, total: 9 }).unwrap();
        assert_eq!(json, serde_json::json!({"done": 3, "total": 9}));
        let json = serde_json::to_value(ReadyPayload {
            model: "m".into(),
            count: 2,
        })
        .unwrap();
        assert_eq!(json, serde_json::json!({"model": "m", "count": 2}));
    }

    #[test]
    fn identity_of_model_uses_manifest() {
        let identity = identity_of_model("ruri-v3-70m").unwrap();
        assert_eq!(identity.model_name, "ruri-v3-70m");
        assert_eq!(identity.dimensions, 384);
        assert_eq!(
            identity.model_revision,
            "07a8b0aba47d29d2ca21f89b915c1efe2c23d1cc"
        );
        // 30mは次元が異なる (混在防止の実質)
        assert_eq!(identity_of_model("ruri-v3-30m").unwrap().dimensions, 256);
        assert!(identity_of_model("unknown-model").is_none());
    }
}
