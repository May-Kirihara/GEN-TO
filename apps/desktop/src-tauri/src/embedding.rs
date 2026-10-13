//! 自然言語検索 (ローカルruri-v3埋め込み) のIPCとワーカー管理 (MVP WP-11)。
//!
//! - [`EmbeddingService`] — 独立ワーカープロセス (`gento-embedding-worker`)
//!   のspawn・再起動 (バックオフ)・短時間連続異常終了での自動再起停止、
//!   埋め込みジョブ (jobs表 `embedding`) の実行、検索文書hash差分への
//!   再enqueue、総当たりcosineデバッグ検索。
//! - command群はasync + spawn_blocking (WP-8/10前例。全件収集級のDB読み
//!   取りをメインスレッドに置かない)。
//! - DTOはcamelCase付きでcrate境界を越えない (FIX-AI-DTO教訓 —
//!   直列化形状テストで契約を固定)。
//!
//! 有効状態・選択モデルは app_settings (migration #9) に永続化する。
//! 初期状態は無効 (設計§1)。モデル検証失敗・不在でもアプリは継続する
//! (acceptance 5-1 — statusのpackageStatesで状態を観測)。

use std::collections::{HashMap, VecDeque};
use std::io::BufReader;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gento_core::jobs::backoff_delay_ms;
use gento_db::{
    rfc3339_after_now, Db, JOB_TYPE_EMBEDDING, SETTING_EMBEDDING_ENABLED, SETTING_EMBEDDING_MODEL,
};
use serde::Serialize;
use tauri::State;

/// 通知送出境界 (Tauri emit / テスト記録)。
type NoticeSink = Arc<dyn Fn(&str, &str) + Send + Sync>;
/// ワーカーspawn境界 (テストで差し替え)。
type WorkerCommandFactory = Box<dyn Fn(&PathBuf) -> Command + Send + Sync>;
/// 埋め込みupsertのフック (WP-12: USearch索引への増分追加)。
pub type VectorSink = Arc<dyn Fn(&str, &str, i64, &[f32]) + Send + Sync>;

use crate::catalog::AppState;

/// 埋め込みジョブの優先度 (docs/embedding-runtime.md §7をjobs表priority
/// へmap。query=キューを経由しない直接実行のため対象外。表示中=WP-12、
/// 再構築=WP-12の全再構築で接続予定)。
const PRIORITY_NEW_ASSET: i64 = 4;
const PRIORITY_INITIAL: i64 = 6;

/// 1バッチあたりの文書数 (バッチ境界でクエリ割込・キャンセルを受け付ける)。
const DOC_BATCH: usize = 32;
/// ジョブポーリング間隔。
const JOB_POLL: Duration = Duration::from_millis(500);
/// 差分掃討 (メタデータ変更の再enqueue判定) の間隔。
const SWEEP_INTERVAL: Duration = Duration::from_secs(30);
/// 1回の掃討でhash比較するasset上限。
const SWEEP_PAGE: usize = 2000;
/// 掃討の1ページ読み取り件数 (present_asset_idsへのlimit)。
const SWEEP_READ_PAGE: u32 = 256;
/// リクエストのタイムアウト (初回ロードはモデル280MBのmmapを伴う)。
const LOAD_TIMEOUT: Duration = Duration::from_secs(120);
const EMBED_TIMEOUT: Duration = Duration::from_secs(300);
/// ワーカー再起動バックオフ (base, factor)。
const RESTART_BACKOFF_BASE_MS: u64 = 1_000;
const RESTART_BACKOFF_FACTOR: u64 = 4;
/// クラッシュループ判定: この窓内にこの回数以上死んだら自動再起を止める。
const CRASH_WINDOW: Duration = Duration::from_secs(120);
const CRASH_THRESHOLD: usize = 3;
/// セルフチェック (検証スコア警告) の類似度マージン閾値。
const SELFCHECK_MIN_MARGIN: f32 = 0.05;

// ===== DTO =====

/// パッケージ状態DTO (`embedding_status`)。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelPackageStateDto {
    pub model_id: String,
    /// "verified" | "missing" | "missing_files" | "mismatch"
    pub state: String,
    /// 日本語説明 (検証失敗理由を含む)。
    pub detail: String,
    pub package_dir: String,
    pub embedding_dim: usize,
    pub license: String,
}

/// ワーカー稼働状態。
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerPhase {
    /// まだ起動していない (lazy spawn)。
    NotStarted,
    Running,
    /// バックオフ待ち (前回異常終了)。
    Restarting,
    /// クラッシュループで自動再起停止 (ユーザー操作で復帰)。
    Crashed,
}

/// `embedding_status` 応答DTO。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingStatusDto {
    pub enabled: bool,
    pub model: String,
    pub models_dir: String,
    pub packages: Vec<ModelPackageStateDto>,
    pub worker: WorkerPhaseDto,
    /// ロード済みモデルID (未ロード = null)。
    pub loaded_model: Option<String>,
    pub embedded_count: u64,
    pub total_count: u64,
    pub pending_jobs: u64,
    /// セルフチェック (近い/遠い文書対のcosineマージン)。未実施 = null。
    pub selfcheck_margin: Option<f32>,
    /// 検証スコアが閾値未満の場合の警告 (acceptance 5-4)。
    pub warning: Option<String>,
}

/// ワーカー状態DTO。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkerPhaseDto {
    pub phase: WorkerPhase,
    pub restart_count: u64,
    pub last_error: Option<String>,
}

/// `embedding_similar_debug` 応答項目。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SimilarItemDto {
    pub asset_id: String,
    pub score: f32,
}

// ===== サービス =====

/// ワーカー死亡イベント (supervisor・status参照用の記録)。
struct DeathLog {
    recent: VecDeque<Instant>,
    restart_count: u64,
    last_error: Option<String>,
}

struct WorkerHandle {
    child: Child,
    stdin: Mutex<Option<ChildStdin>>,
    generation: u64,
    /// 正常shutdown要請済み (EOF時は異常終了と数えない)。
    shutting_down: bool,
}

/// ロード済みモデル (status・二重ロード回避)。
#[derive(Clone)]
struct LoadedModel {
    model_id: String,
    info: gento_embedding::ProtoModelInfo,
}

pub struct EmbeddingService {
    db: Arc<Db>,
    models_dir: PathBuf,
    worker_path: PathBuf,
    enabled: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    next_id: AtomicU64,
    pending: Arc<Mutex<HashMap<u64, SyncSender<gento_embedding::WorkerResponse>>>>,
    worker: Arc<Mutex<Option<WorkerHandle>>>,
    deaths: Arc<Mutex<DeathLog>>,
    /// 次のspawn許可時刻 (バックオフ)。
    next_spawn_allowed: Arc<Mutex<Instant>>,
    loaded: Arc<Mutex<Option<LoadedModel>>>,
    selfcheck: Arc<Mutex<Option<f32>>>,
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
    /// ワーカーspawn境界 (テストで差し替え)。
    worker_command: Box<dyn Fn(&PathBuf) -> Command + Send + Sync>,
    /// 通知送出境界 (Tauri emit / テスト記録)。
    emit: NoticeSink,
    /// クラッシュ停止通知済みフラグ (重複通知防止)。
    crash_notified: AtomicBool,
    /// モデルロードの直列化 (ハイブリッド検索からの同時ensure防止。WP-12)。
    load_gate: Mutex<()>,
    /// 埋め込みupsertフック (WP-12: USearch索引増分追加。setup後に接続)。
    vector_sink: Mutex<Option<VectorSink>>,
    /// 差分掃討の走査位置 (最後に検査したasset_id。0=先頭から)。
    /// 1回の掃討はSWEEP_PAGE上限で途中までしか進まないため、終端到達で
    /// 0へ巻き戻しつつ次回はこの位置から継続する — 常時先頭走査だと
    /// id順末尾のassetが毎回上限の裏に隠れて永久に検査されない。
    sweep_cursor: AtomicI64,
}

impl EmbeddingService {
    pub fn start(db: Arc<Db>, models_dir: PathBuf, emit: NoticeSink) -> Arc<Self> {
        Self::start_with(db, models_dir, emit, default_worker_command())
    }

    /// テスト用: ワーカーspawn境界を差し替える。
    pub fn start_with(
        db: Arc<Db>,
        models_dir: PathBuf,
        emit: NoticeSink,
        worker_command: Box<dyn Fn(&PathBuf) -> Command + Send + Sync>,
    ) -> Arc<Self> {
        // 設定復元 (失敗時は既定: 無効・70m)
        let (enabled, model) = db
            .with_reader(|repos| {
                let enabled = repos
                    .setting_get(SETTING_EMBEDDING_ENABLED)
                    .ok()
                    .flatten()
                    .is_some_and(|v| v == "1");
                let model = repos
                    .setting_get(SETTING_EMBEDDING_MODEL)
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| gento_embedding::MODEL_RURI_V3_70M.to_string());
                Ok((enabled, model))
            })
            .unwrap_or((false, gento_embedding::MODEL_RURI_V3_70M.to_string()));
        // 選択モデル設定の正本への反映 (未設定時のみ)
        if db
            .with_reader(|repos| repos.setting_get(SETTING_EMBEDDING_MODEL))
            .ok()
            .flatten()
            .is_none()
        {
            let _ = db.with_writer(|repos| repos.setting_set(SETTING_EMBEDDING_MODEL, &model));
        }
        let service = Arc::new(Self {
            worker_path: resolve_worker_path(),
            db,
            models_dir,
            enabled: Arc::new(AtomicBool::new(enabled)),
            shutdown: Arc::new(AtomicBool::new(false)),
            next_id: AtomicU64::new(1),
            pending: Arc::new(Mutex::new(HashMap::new())),
            worker: Arc::new(Mutex::new(None)),
            deaths: Arc::new(Mutex::new(DeathLog {
                recent: VecDeque::new(),
                restart_count: 0,
                last_error: None,
            })),
            next_spawn_allowed: Arc::new(Mutex::new(Instant::now() - Duration::from_secs(1))),
            loaded: Arc::new(Mutex::new(None)),
            selfcheck: Arc::new(Mutex::new(None)),
            threads: Mutex::new(Vec::new()),
            worker_command,
            emit: Arc::clone(&emit),
            crash_notified: AtomicBool::new(false),
            load_gate: Mutex::new(()),
            vector_sink: Mutex::new(None),
            sweep_cursor: AtomicI64::new(0),
        });
        // ジョブ実行スレッド (enabled時のみ仕事をする)
        {
            let service2 = Arc::clone(&service);
            let emit2 = Arc::clone(&service.emit);
            let handle = std::thread::Builder::new()
                .name("embedding-jobs".into())
                .spawn(move || job_loop(&service2, &emit2))
                .expect("spawn embedding job loop");
            service
                .threads
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(handle);
        }
        service
    }

    pub fn models_dir(&self) -> &std::path::Path {
        &self.models_dir
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::SeqCst);
        let value = if enabled { "1" } else { "0" };
        let _ = self
            .db
            .with_writer(|repos| repos.setting_set(SETTING_EMBEDDING_ENABLED, value));
        if !enabled {
            // 無効化: ロード中モデルをアンロード (ワーカーは次のアイドル判定へ)
            let _ = self.call(
                gento_embedding::WorkerRequest::Unload { id: 0 },
                Duration::from_secs(10),
            );
            *self.loaded.lock().unwrap_or_else(|p| p.into_inner()) = None;
            *self.selfcheck.lock().unwrap_or_else(|p| p.into_inner()) = None;
        }
    }

    /// 選択モデル (app_settings正本から都度読み出し)。
    pub fn selected_model(&self) -> String {
        self.db
            .with_reader(|repos| repos.setting_get(SETTING_EMBEDDING_MODEL))
            .ok()
            .flatten()
            .unwrap_or_else(|| gento_embedding::MODEL_RURI_V3_70M.to_string())
    }

    pub fn set_selected_model(&self, model: &str) -> Result<(), String> {
        if !gento_embedding::is_known_model(model) {
            return Err(format!("不明なモデルIDです: {model}"));
        }
        self.db
            .with_writer(|repos| repos.setting_set(SETTING_EMBEDDING_MODEL, model))
            .map_err(|e| e.to_string())?;
        // モデル変更はロード済みベクトルの切り替えを意味する — アンロードして
        // 次ループで新モデルをロードさせる (§10: 切り替え中も旧検索継続は
        // WP-12のUSearch世代管理で扱う)
        *self.loaded.lock().unwrap_or_else(|p| p.into_inner()) = None;
        Ok(())
    }

    /// Graceful停止 (Exit時)。
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(handle) = self
            .worker
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_mut()
        {
            handle.shutting_down = true;
            let _ = self.call(
                gento_embedding::WorkerRequest::Shutdown { id: 0 },
                Duration::from_secs(5),
            );
        }
        // stdinを閉じてreader終了 → プロセス終了
        if let Some(handle) = self
            .worker
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_mut()
        {
            *handle.stdin.lock().unwrap_or_else(|p| p.into_inner()) = None;
        }
    }

    /// 死亡記録とバックオフ更新 (書き込み失敗経路。readerスレッドの
    /// EOF検知は [`record_worker_death`] を直接呼ぶ)。
    fn record_death(&self, generation: u64, error: String) {
        record_worker_death(
            &self.deaths,
            &self.worker,
            &self.next_spawn_allowed,
            &self.loaded,
            generation,
            error,
        );
    }

    /// ワーカーが必要ならspawnする (lazy起動 + バックオフ + クラッシュループガード)。
    /// 戻り値: Ok(()) = 生存確認。Err = ステータス理由。
    fn ensure_worker(&self) -> Result<(), String> {
        if self.shutdown.load(Ordering::SeqCst) {
            return Err("シャットダウン中です".to_string());
        }
        {
            let mut worker = self.worker.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(handle) = worker.as_mut() {
                if matches!(handle.child.try_wait(), Ok(None)) {
                    return Ok(()); // 生存
                }
            }
        }
        // クラッシュループ判定 (バックオフより優先 — 停止状態の理由を
        // 「待ち」ではなく「停止」として利用者に伝える)
        {
            let deaths = self.deaths.lock().unwrap_or_else(|p| p.into_inner());
            if deaths.recent.len() >= CRASH_THRESHOLD {
                let message = format!(
                    "ワーカーが短時間に{}回異常終了したため自動再起動を停止しました (設定から再度有効化してください)",
                    deaths.recent.len()
                );
                if !self.crash_notified.swap(true, Ordering::SeqCst) {
                    (self.emit)("embedding:notice", &message);
                }
                return Err(message);
            }
        }
        // spawnバックオフ
        {
            let allowed = *self
                .next_spawn_allowed
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if Instant::now() < allowed {
                return Err("ワーカー再起動のバックオフ待ちです".to_string());
            }
        }
        let mut child = (self.worker_command)(&self.worker_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("ワーカー起動失敗 ({}): {e}", self.worker_path.display()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "ワーカーのstdinを取得できません".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "ワーカーのstdoutを取得できません".to_string())?;
        let generation = self
            .deaths
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .restart_count
            + 1;
        // readerスレッド: 応答をpendingへ配送し、EOF (ワーカー終了) で
        // 死亡記録 + バックオフ設定 + クラッシュループ計数を行う
        {
            let pending = Arc::clone(&self.pending);
            let deaths = Arc::clone(&self.deaths);
            let worker_slot = Arc::clone(&self.worker);
            let next_allowed = Arc::clone(&self.next_spawn_allowed);
            let loaded = Arc::clone(&self.loaded);
            let reader = BufReader::new(stdout);
            std::thread::Builder::new()
                .name(format!("embed-reader-{generation}"))
                .spawn(move || {
                    let mut reader = reader;
                    loop {
                        match gento_embedding::worker_proto::read_frame::<
                            gento_embedding::WorkerResponse,
                        >(&mut reader)
                        {
                            Ok(Some(response)) => {
                                let mut pending = pending.lock().unwrap_or_else(|p| p.into_inner());
                                if let Some(sender) = pending.remove(&response.id) {
                                    let _ = sender.send(response);
                                }
                            }
                            Ok(None) => break,
                            Err(_) => break,
                        }
                    }
                    // ワーカー終了: pendingを全解散 (待機者はタイムアウトするが
                    // ここで即座に返すため空応答ではなく単にdrop)
                    pending.lock().unwrap_or_else(|p| p.into_inner()).clear();
                    // 正常shutdown要請済みなら死亡に数えない
                    let requested_shutdown = worker_slot
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .as_ref()
                        .is_some_and(|h| h.generation == generation && h.shutting_down);
                    if !requested_shutdown {
                        record_worker_death(
                            &deaths,
                            &worker_slot,
                            &next_allowed,
                            &loaded,
                            generation,
                            "ワーカープロセスが終了しました".to_string(),
                        );
                    }
                })
                .expect("spawn embed reader");
        }
        let mut worker = self.worker.lock().unwrap_or_else(|p| p.into_inner());
        // 古いワーカーが残っていれば掃除 (kill)
        if let Some(old) = worker.as_mut() {
            let _ = old.child.kill();
        }
        *worker = Some(WorkerHandle {
            child,
            stdin: Mutex::new(Some(stdin)),
            generation,
            shutting_down: false,
        });
        Ok(())
    }

    /// ワーカーへリクエストを送り応答を待つ。
    fn call(
        &self,
        mut request: gento_embedding::WorkerRequest,
        timeout: Duration,
    ) -> Result<gento_embedding::WorkerResponse, String> {
        self.ensure_worker()?;
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        // idを差し込む (cmd毎にフィールドが違うため書き換え)
        request = with_request_id(request, id);
        let (tx, rx): (
            SyncSender<gento_embedding::WorkerResponse>,
            Receiver<gento_embedding::WorkerResponse>,
        ) = std::sync::mpsc::sync_channel(1);
        self.pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(id, tx);
        let write_result = {
            let worker = self.worker.lock().unwrap_or_else(|p| p.into_inner());
            match worker.as_ref() {
                Some(handle) => {
                    let mut stdin = handle.stdin.lock().unwrap_or_else(|p| p.into_inner());
                    match stdin.as_mut() {
                        Some(stdin) => gento_embedding::worker_proto::write_frame(stdin, &request),
                        None => Err("ワーカーは停止済みです".to_string()),
                    }
                }
                None => Err("ワーカーが起動していません".to_string()),
            }
        };
        if let Err(error) = write_result {
            self.pending
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&id);
            // 書き込み失敗 = ワーカー死亡相当
            self.record_death(0, error.clone());
            return Err(error);
        }
        match rx.recv_timeout(timeout) {
            Ok(response) => Ok(response),
            Err(_) => {
                self.pending
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(&id);
                Err("ワーカー応答タイムアウト".to_string())
            }
        }
    }

    /// 埋め込みupsertフックを接続する (lib.rs setupでVectorIndexService
    /// 生成後に呼ぶ)。
    pub fn set_vector_sink(&self, sink: VectorSink) {
        *self.vector_sink.lock().unwrap_or_else(|p| p.into_inner()) = Some(sink);
    }

    fn notify_vector_sink(&self, model: &str, revision: &str, asset_id: i64, vector: &[f32]) {
        let sink = self
            .vector_sink
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        if let Some(sink) = sink {
            sink(model, revision, asset_id, vector);
        }
    }

    /// モデルを検証してロードする。成功時セルフチェックを実施する。
    pub fn ensure_loaded(&self) -> Result<gento_embedding::ProtoModelInfo, String> {
        // 直列化: ハイブリッド検索とジョブログが同時にロードを競合させない
        let _gate = self.load_gate.lock().unwrap_or_else(|p| p.into_inner());
        let model = self.selected_model();
        let already = self
            .loaded
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        if let Some(loaded) = already {
            if loaded.model_id == model {
                return Ok(loaded.info);
            }
        }
        let verification = gento_embedding::manifest::verify_package(&self.models_dir, &model);
        if !verification.is_verified() {
            return Err(format!(
                "モデルパッケージが検証をパスしません ({model}): {}",
                verification.describe()
            ));
        }
        let threads = std::env::var("GENTO_EMBEDDING_THREADS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok());
        let response = self.call(
            gento_embedding::WorkerRequest::Load {
                id: 0,
                model: model.clone(),
                models_dir: self.models_dir.to_string_lossy().to_string(),
                threads,
            },
            LOAD_TIMEOUT,
        )?;
        if !response.ok {
            return Err(response.error.unwrap_or_else(|| "ロード失敗".to_string()));
        }
        let info = match response.result {
            Some(gento_embedding::WorkerResult::Load { model_info }) => model_info,
            _ => return Err("ロード応答の形状が不正です".to_string()),
        };
        *self.loaded.lock().unwrap_or_else(|p| p.into_inner()) = Some(LoadedModel {
            model_id: model,
            info: info.clone(),
        });
        // ロード成功 = 安定稼働: クラッシュ窓の計数をリセット
        self.deaths
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .recent
            .clear();
        // セルフチェック (acceptance 5-4): 近い/遠い対のcosineマージン
        let _ = self.run_selfcheck();
        Ok(info)
    }

    /// セルフチェック: 固定対で近い文書が遠い文書より十分高い類似度か。
    fn run_selfcheck(&self) -> Option<f32> {
        let query = "可愛い猫のイラスト";
        let near = "子猫のかわいいイラスト。猫が遊んでいる絵。";
        let far = "銀行の決算報告書の表。数値とグラフ。";
        let q = self
            .call(
                gento_embedding::WorkerRequest::EmbedQuery {
                    id: 0,
                    text: query.to_string(),
                },
                EMBED_TIMEOUT,
            )
            .ok()?;
        let docs = self
            .call(
                gento_embedding::WorkerRequest::EmbedDocuments {
                    id: 0,
                    texts: vec![near.to_string(), far.to_string()],
                },
                EMBED_TIMEOUT,
            )
            .ok()?;
        let (q_vec, near_vec, far_vec) = match (q.result, docs.result) {
            (
                Some(gento_embedding::WorkerResult::EmbedQuery { vector }),
                Some(gento_embedding::WorkerResult::EmbedDocuments { vectors }),
            ) => (vector, vectors.first()?.clone(), vectors.get(1)?.clone()),
            _ => return None,
        };
        let cosine = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        let margin = cosine(&q_vec, &near_vec) - cosine(&q_vec, &far_vec);
        *self.selfcheck.lock().unwrap_or_else(|p| p.into_inner()) = Some(margin);
        Some(margin)
    }

    /// 1クエリの埋め込み (similar_debug・将来の検索バー)。ロードを保証する。
    pub fn embed_query(&self, text: &str) -> Result<Vec<f32>, String> {
        self.ensure_loaded()?;
        let response = self.call(
            gento_embedding::WorkerRequest::EmbedQuery {
                id: 0,
                text: text.to_string(),
            },
            EMBED_TIMEOUT,
        )?;
        if !response.ok {
            return Err(response.error.unwrap_or_else(|| "埋め込み失敗".to_string()));
        }
        match response.result {
            Some(gento_embedding::WorkerResult::EmbedQuery { vector }) => Ok(vector),
            _ => Err("埋め込み応答の形状が不正です".to_string()),
        }
    }

    /// 文書バッチ埋め込み (ジョブループから使用)。
    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        self.ensure_loaded()?;
        let response = self.call(
            gento_embedding::WorkerRequest::EmbedDocuments {
                id: 0,
                texts: texts.to_vec(),
            },
            EMBED_TIMEOUT,
        )?;
        if !response.ok {
            return Err(response.error.unwrap_or_else(|| "埋め込み失敗".to_string()));
        }
        match response.result {
            Some(gento_embedding::WorkerResult::EmbedDocuments { vectors }) => Ok(vectors),
            _ => Err("埋め込み応答の形状が不正です".to_string()),
        }
    }

    /// ワーカーを強制殺す (E2Eテスト用・実行中ジョブの再試行経路検証)。
    pub fn kill_worker_for_test(&self) -> bool {
        let mut worker = self.worker.lock().unwrap_or_else(|p| p.into_inner());
        match worker.as_mut() {
            Some(handle) => {
                let killed = handle.child.kill().is_ok();
                *worker = None;
                killed
            }
            None => false,
        }
    }

    /// ワーカーへstatus要求 (稼働確認・テストのspawn駆動)。未起動なら
    /// ensure_workerを経由してspawnを試みる。
    pub fn worker_status(&self) -> Result<gento_embedding::WorkerStatus, String> {
        let response = self.call(
            gento_embedding::WorkerRequest::Status { id: 0 },
            Duration::from_secs(10),
        )?;
        if !response.ok {
            return Err(response.error.unwrap_or_else(|| "status失敗".to_string()));
        }
        match response.result {
            Some(gento_embedding::WorkerResult::Status { status }) => Ok(status),
            _ => Err("status応答の形状が不正です".to_string()),
        }
    }

    /// クラッシュループ停止状態の復帰 (enable時に呼ばれる。手動復帰用に公開)。
    pub fn reset_crash_state(&self) {
        self.crash_notified.store(false, Ordering::SeqCst);
        self.deaths
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .recent
            .clear();
        *self
            .next_spawn_allowed
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Instant::now();
    }

    /// 状態スナップショット。
    pub fn status(&self) -> EmbeddingStatusDto {
        let packages = gento_embedding::model_store::scan_models(&self.models_dir)
            .into_iter()
            .map(|state| {
                let spec = gento_embedding::manifest::model_spec(&state.model_id).ok();
                let phase = match state.verification {
                    gento_embedding::PackageVerification::Verified => "verified",
                    gento_embedding::PackageVerification::Missing => "missing",
                    gento_embedding::PackageVerification::MissingFiles(_) => "missing_files",
                    gento_embedding::PackageVerification::Mismatch { .. } => "mismatch",
                };
                ModelPackageStateDto {
                    model_id: state.model_id,
                    state: phase.to_string(),
                    detail: state.verification.describe(),
                    package_dir: state.package_dir.to_string_lossy().to_string(),
                    embedding_dim: spec.as_ref().map(|s| s.embedding_dim).unwrap_or(0),
                    license: spec
                        .map(|s| s.license)
                        .unwrap_or_else(|| "unknown".to_string()),
                }
            })
            .collect();
        let model = self.selected_model();
        let (embedded, total, pending) = {
            self.db
                .with_reader(|repos| {
                    let revision = model_revision_of(&model);
                    let embedded = repos
                        .embedding_count_for_model(&model, &revision)
                        .unwrap_or(0);
                    let total = repos.embeddable_asset_count().unwrap_or(0);
                    let pending = repos
                        .pending_job_count_of_type(JOB_TYPE_EMBEDDING)
                        .unwrap_or(0);
                    Ok((embedded, total, pending))
                })
                .unwrap_or((0, 0, 0))
        };
        let (phase, restart_count, last_error) = {
            let deaths = self.deaths.lock().unwrap_or_else(|p| p.into_inner());
            let alive = self
                .worker
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_mut()
                .is_some_and(|h| matches!(h.child.try_wait(), Ok(None)));
            let phase = if deaths.recent.len() >= CRASH_THRESHOLD {
                WorkerPhase::Crashed
            } else if alive {
                WorkerPhase::Running
            } else if deaths.restart_count > 0 {
                WorkerPhase::Restarting
            } else {
                WorkerPhase::NotStarted
            };
            (phase, deaths.restart_count, deaths.last_error.clone())
        };
        let margin = *self.selfcheck.lock().unwrap_or_else(|p| p.into_inner());
        let warning = match margin {
            Some(m) if m < SELFCHECK_MIN_MARGIN => Some(format!(
                "埋め込み品質のセルフチェックが閾値を下回っています (マージン {m:.3} < {SELFCHECK_MIN_MARGIN})。モデルの再取得を検討してください"
            )),
            _ => None,
        };
        EmbeddingStatusDto {
            enabled: self.enabled.load(Ordering::SeqCst),
            model,
            models_dir: self.models_dir.to_string_lossy().to_string(),
            packages,
            worker: WorkerPhaseDto {
                phase,
                restart_count,
                last_error,
            },
            loaded_model: self
                .loaded
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_ref()
                .map(|loaded| loaded.model_id.clone()),
            embedded_count: embedded,
            total_count: total,
            pending_jobs: pending,
            selfcheck_margin: margin,
            warning,
        }
    }

    /// 有効化 (モデル検証 → 設定保存 → 初回掃討)。検証失敗時は有効化しない
    /// (acceptance 5-1: アプリは継続し、状態はstatusで観測)。
    pub fn enable(&self, model: Option<&str>) -> Result<EmbeddingStatusDto, String> {
        if let Some(model) = model {
            self.set_selected_model(model)?;
        }
        let model = self.selected_model();
        let verification = gento_embedding::manifest::verify_package(&self.models_dir, &model);
        if !verification.is_verified() {
            return Err(format!(
                "モデルパッケージが検証をパスしません ({model}): {}",
                verification.describe()
            ));
        }
        self.set_enabled(true);
        // クラッシュループ停止状態からの復帰 (手動操作)
        self.reset_crash_state();
        let enqueued = self.sweep_enqueues_pending(SWEEP_PAGE);
        tracing::info!(enqueued, "embedding enabled; initial sweep done");
        Ok(self.status())
    }

    /// 無効化。
    pub fn disable(&self) -> EmbeddingStatusDto {
        self.set_enabled(false);
        self.status()
    }

    /// モデル削除 (ベクトル + パッケージ)。モデル削除後も通常検索は動作。
    /// USearch索引の破棄はcommand層がVectorIndexServiceへ依頼する (WP-12)。
    pub fn delete_model(&self, model: &str) -> Result<EmbeddingStatusDto, String> {
        let removed = self
            .db
            .with_writer(|repos| repos.embedding_delete_for_model(model))
            .map_err(|e| e.to_string())?;
        gento_embedding::model_store::delete_package(&self.models_dir, model)
            .map_err(|e| e.to_string())?;
        tracing::info!(model = %model, vectors = removed, "embedding model deleted");
        Ok(self.status())
    }

    /// 総当たりcosineデバッグ検索 (WP-12のUSearch前の検証・acceptance用。
    /// UI非公開)。
    pub fn similar_debug(
        &self,
        asset_id: Option<i64>,
        query_text: Option<&str>,
        k: u32,
    ) -> Result<Vec<SimilarItemDto>, String> {
        let k = k.clamp(1, 100);
        let model = self.selected_model();
        let revision = model_revision_of(&model);
        let query_vector: Vec<f32> = if let Some(asset_id) = asset_id {
            self.db
                .with_reader(|repos| {
                    Ok(repos
                        .embedding_vectors_for_model(&model, &revision)?
                        .into_iter()
                        .find(|(id, _)| *id == asset_id)
                        .map(|(_, v)| v))
                })
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "指定assetの埋め込みがありません".to_string())?
        } else {
            let text = query_text.ok_or_else(|| {
                "asset_id または query_text のいずれかを指定してください".to_string()
            })?;
            self.embed_query(text)?
        };
        let vectors = self
            .db
            .with_reader(|repos| repos.embedding_vectors_for_model(&model, &revision))
            .map_err(|e| e.to_string())?;
        let mut scored: Vec<(i64, f32)> = vectors
            .into_iter()
            .map(|(id, vector)| {
                let score = query_vector
                    .iter()
                    .zip(&vector)
                    .map(|(a, b)| a * b)
                    .sum::<f32>();
                (id, score)
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        Ok(scored
            .into_iter()
            .take(k as usize)
            .map(|(asset_id, score)| SimilarItemDto {
                asset_id: asset_id.to_string(),
                score,
            })
            .collect())
    }

    /// 差分掃討: 検索文書hashが変わった (または未埋め込みの) assetをenqueue。
    /// 戻り値 = 投入件数。
    ///
    /// 走査は前回終了位置 ([`Self::sweep_cursor`]) からの継続走査で、
    /// テーブル終端 (空ページ) で先頭へ巻き戻す。1回の呼び出しで
    /// `limit`+繰上げ件数しか検査しないため、定期スイープ (SWEEP_INTERVAL)
    /// の繰り返しでライブラリ全体を巡回する。
    pub fn sweep_enqueues_pending(&self, limit: usize) -> usize {
        let model = self.selected_model();
        let revision = model_revision_of(&model);
        // 初回 (モデルの埋め込みが0件) は低優先度、以降の差分は新規優先度
        let priority = if self
            .db
            .with_reader(|repos| repos.embedding_count_for_model(&model, &revision))
            .unwrap_or(0)
            == 0
        {
            PRIORITY_INITIAL
        } else {
            PRIORITY_NEW_ASSET
        };
        let mut enqueued = 0usize;
        let mut after_id = self.sweep_cursor.load(Ordering::SeqCst);
        let mut examined = 0usize;
        while examined < limit {
            let page: Vec<i64> = self
                .db
                .with_reader(|repos| repos.present_asset_ids(after_id, SWEEP_READ_PAGE))
                .unwrap_or_default();
            if page.is_empty() {
                // テーブル終端: 次回は先頭から (前半の差分再検査の巡回)
                self.sweep_cursor.store(0, Ordering::SeqCst);
                break;
            }
            after_id = *page.last().unwrap();
            examined += page.len();
            // limit到達で打ち切った位置を次回の継続地点へ保存
            // (最後のページが同時に終端でも次回の空ページで巻き戻る)
            self.sweep_cursor.store(after_id, Ordering::SeqCst);
            let work: Vec<(i64, String)> = self
                .db
                .with_reader(
                    |repos| -> Result<Vec<(i64, String)>, gento_core::repo::RepoError> {
                        Ok(page
                            .into_iter()
                            .filter_map(|asset_id| {
                                let input =
                                    gento_db::build_search_doc_input(repos.conn(), asset_id)
                                        .ok()??;
                                let document = gento_core::compose_search_document_default(&input);
                                if document.is_empty() {
                                    return None;
                                }
                                let hash = blake3_hex(&document);
                                let unchanged = repos
                                    .embedding_meta_for(asset_id, &model, &revision)
                                    .ok()
                                    .flatten()
                                    .is_some_and(|meta| meta.source_hash == hash);
                                if unchanged {
                                    None
                                } else {
                                    Some((asset_id, hash))
                                }
                            })
                            .collect())
                    },
                )
                .unwrap_or_default();
            for (asset_id, hash) in work {
                let inserted = self.db.with_writer(|repos| {
                    repos.enqueue_job(
                        JOB_TYPE_EMBEDDING,
                        "asset",
                        asset_id,
                        priority,
                        &format!("{{\"source_hash\":\"{hash}\"}}"),
                    )
                });
                if inserted.unwrap_or(None).is_some() {
                    enqueued += 1;
                }
            }
        }
        enqueued
    }
}

impl Drop for EmbeddingService {
    fn drop(&mut self) {
        self.shutdown();
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

/// ワーカー死亡の記録: クラッシュ窓内計数・バックオフ設定・世代除去。
fn record_worker_death(
    deaths: &Arc<Mutex<DeathLog>>,
    worker_slot: &Arc<Mutex<Option<WorkerHandle>>>,
    next_allowed: &Arc<Mutex<Instant>>,
    loaded: &Arc<Mutex<Option<LoadedModel>>>,
    generation: u64,
    error: String,
) {
    let backoff_ms;
    {
        let mut deaths = deaths.lock().unwrap_or_else(|p| p.into_inner());
        deaths.recent.push_back(Instant::now());
        while deaths
            .recent
            .front()
            .is_some_and(|t| t.elapsed() > CRASH_WINDOW)
        {
            deaths.recent.pop_front();
        }
        deaths.restart_count += 1;
        deaths.last_error = Some(error);
        backoff_ms = backoff_delay_ms(
            deaths.restart_count as u32,
            RESTART_BACKOFF_BASE_MS,
            RESTART_BACKOFF_FACTOR,
            0.5,
        );
    }
    let mut worker = worker_slot.lock().unwrap_or_else(|p| p.into_inner());
    if worker.as_ref().is_some_and(|h| h.generation == generation) {
        *worker = None;
    }
    drop(worker);
    *loaded.lock().unwrap_or_else(|p| p.into_inner()) = None;
    *next_allowed.lock().unwrap_or_else(|p| p.into_inner()) =
        Instant::now() + Duration::from_millis(backoff_ms);
}

fn blake3_hex(text: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(text.as_bytes());
    hasher.finalize().to_hex().to_string()
}

/// manifest revision (埋め込み行のmodel_revision値 = 元モデルrevision)。
pub fn model_revision_of(model_id: &str) -> String {
    gento_embedding::manifest::model_spec(model_id)
        .map(|spec| spec.revision)
        .unwrap_or_else(|_| "unknown".to_string())
}

/// app_settingsから選択モデルを直接読む (サービスを介さない経路 —
/// WP-12の類似画像検索等)。
pub fn selected_model_of(db: &Arc<Db>) -> String {
    db.with_reader(|repos| repos.setting_get(SETTING_EMBEDDING_MODEL))
        .ok()
        .flatten()
        .unwrap_or_else(|| gento_embedding::MODEL_RURI_V3_70M.to_string())
}

fn with_request_id(
    request: gento_embedding::WorkerRequest,
    id: u64,
) -> gento_embedding::WorkerRequest {
    match request {
        gento_embedding::WorkerRequest::Load {
            model,
            models_dir,
            threads,
            ..
        } => gento_embedding::WorkerRequest::Load {
            id,
            model,
            models_dir,
            threads,
        },
        gento_embedding::WorkerRequest::Unload { .. } => {
            gento_embedding::WorkerRequest::Unload { id }
        }
        gento_embedding::WorkerRequest::EmbedQuery { text, .. } => {
            gento_embedding::WorkerRequest::EmbedQuery { id, text }
        }
        gento_embedding::WorkerRequest::EmbedDocuments { texts, .. } => {
            gento_embedding::WorkerRequest::EmbedDocuments { id, texts }
        }
        gento_embedding::WorkerRequest::Status { .. } => {
            gento_embedding::WorkerRequest::Status { id }
        }
        gento_embedding::WorkerRequest::Shutdown { .. } => {
            gento_embedding::WorkerRequest::Shutdown { id }
        }
    }
}

/// ワーカーバイナリの解決: 1) GENTO_EMBEDDING_WORKER環境変数 (テスト・
/// E2E) 2) 自バイナリと同ディレクトリ (cargo target/{debug,release}・
/// バンドル時のsidecar配置の両方をカバー)。
/// Windowsでは `.exe` 付きで探索する (EXE_SUFFIX — Unixでは空文字列)。
pub fn resolve_worker_path() -> PathBuf {
    if let Ok(path) = std::env::var("GENTO_EMBEDDING_WORKER") {
        return PathBuf::from(path);
    }
    let worker_name = format!("gento-embedding-worker{}", std::env::consts::EXE_SUFFIX);
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(&worker_name);
            if candidate.is_file() {
                return candidate;
            }
            // cargo統合テストは target/deps/ 配下で実行されるため
            // 親 (target/{debug,release}) も候補にする。
            if dir
                .file_name()
                .is_some_and(|n| n == "deps" || n == "examples")
            {
                if let Some(target_dir) = dir.parent() {
                    let candidate = target_dir.join(&worker_name);
                    if candidate.is_file() {
                        return candidate;
                    }
                }
            }
        }
    }
    // 最終fallback (PATH検索): WindowsのCreateProcessは拡張子を補完する
    // ためサフィックスなしでも動くが、候補名と一貫させるため同じ形式。
    PathBuf::from(worker_name)
}

/// アプリsetupでのmodels_dir解決: `--models-dir` → `GENTO_MODELS_DIR` →
/// `AppDirs.data_dir/models` (WP-11設計§1)。
pub fn resolve_models_dir_for_app(args: &[String], app_dirs: &gento_core::AppDirs) -> PathBuf {
    gento_embedding::model_store::resolve_models_dir_from_env(args, &app_dirs.data_dir)
}

/// Tauriイベント送出境界 (`embedding:notice` — クラッシュ通知等)。
pub fn tauri_notice_sink(app: tauri::AppHandle) -> NoticeSink {
    Arc::new(move |event: &str, payload: &str| {
        use tauri::Emitter;
        let _ = app.emit(event, payload);
    })
}

fn default_worker_command() -> WorkerCommandFactory {
    Box::new(|path: &PathBuf| {
        let mut command = Command::new(path);
        command.env_remove("GENTO_EMBEDDING_WORKER");
        command
    })
}

// ===== ジョブログ =====

/// 埋め込みジョブログ (ワーカースレッド本体)。テストから直接呼べる
/// `run_one_batch` を分離してある。
fn job_loop(service: &Arc<EmbeddingService>, emit: &NoticeSink) {
    let mut last_sweep = Instant::now() - SWEEP_INTERVAL;
    loop {
        if service.shutdown.load(Ordering::SeqCst) {
            break;
        }
        if !service.enabled.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1000));
            continue;
        }
        let did_work = run_one_batch(service, emit);
        if !did_work {
            if last_sweep.elapsed() >= SWEEP_INTERVAL {
                let enqueued = service.sweep_enqueues_pending(SWEEP_PAGE);
                if enqueued > 0 {
                    tracing::info!(count = enqueued, "embedding sweep enqueued pending assets");
                }
                last_sweep = Instant::now();
            }
            std::thread::sleep(JOB_POLL);
        }
    }
}

/// 1バッチ分のジョブ実行。戻り値 = 何か処理したか。
fn run_one_batch(service: &Arc<EmbeddingService>, emit: &NoticeSink) -> bool {
    let now = rfc3339_after_now(0);
    let mut jobs = Vec::new();
    while jobs.len() < DOC_BATCH {
        let claimed = service
            .db
            .with_writer(|repos| repos.claim_next_job_of_type(&now, JOB_TYPE_EMBEDDING))
            .unwrap_or(None);
        match claimed {
            Some(job) => jobs.push(job),
            None => break,
        }
    }
    if jobs.is_empty() {
        return false;
    }
    let model = service.selected_model();
    let revision = model_revision_of(&model);
    // 文書構築 (hash比較でスキップ判定)
    let mut embed_jobs: Vec<(gento_db::JobRecord, String, Vec<f32>)> = Vec::new();
    for job in &jobs {
        let asset_id = job.target_id;
        let prepared = service.db.with_reader(|repos| {
            let input = match gento_db::build_search_doc_input(repos.conn(), asset_id)? {
                Some(input) => input,
                None => return Ok::<Option<(String, String)>, gento_core::repo::RepoError>(None),
            };
            let document = gento_core::compose_search_document_default(&input);
            Ok(if document.is_empty() {
                None
            } else {
                Some((document, input.prompt.unwrap_or_default()))
            })
        });
        let prepared = match prepared {
            Ok(p) => p,
            Err(error) => {
                let _ = service.db.with_writer(|repos| {
                    repos.fail_job_permanent(job.id, &format!("文書構築失敗: {error}"))
                });
                continue;
            }
        };
        let Some((document, _prompt)) = prepared else {
            // 文書なし (asset削除・present喪失) — ジョブは完了扱い
            let _ = service.db.with_writer(|repos| repos.complete_job(job.id));
            continue;
        };
        let hash = blake3_hex(&document);
        let unchanged = service
            .db
            .with_reader(|repos| {
                Ok(repos
                    .embedding_meta_for(asset_id, &model, &revision)
                    .ok()
                    .flatten()
                    .is_some_and(|meta| meta.source_hash == hash))
            })
            .unwrap_or(false);
        if unchanged {
            let _ = service.db.with_writer(|repos| repos.complete_job(job.id));
            continue;
        }
        embed_jobs.push((job.clone(), document, Vec::new()));
    }
    if !embed_jobs.is_empty() {
        let texts: Vec<String> = embed_jobs.iter().map(|(_, doc, _)| doc.clone()).collect();
        match service.embed_documents(&texts) {
            Ok(vectors) => {
                for ((job, _, _), vector) in embed_jobs.iter().zip(vectors) {
                    let hash = payload_hash_of(job);
                    let result = service.db.with_writer(|repos| {
                        repos.embedding_upsert(
                            job.target_id,
                            &model,
                            &revision,
                            vector.len(),
                            "f32",
                            &hash,
                            &vector,
                        )
                    });
                    match result {
                        Ok(_) => {
                            let _ = service.db.with_writer(|repos| repos.complete_job(job.id));
                            // WP-12: 索引への増分反映 (索引稼働時のみ効果あり)
                            service.notify_vector_sink(&model, &revision, job.target_id, &vector);
                        }
                        Err(error) => {
                            let retry_at = rfc3339_after_now(backoff_delay_ms(
                                job.attempts,
                                gento_core::jobs::DEFAULT_BACKOFF_BASE_MS,
                                gento_core::jobs::DEFAULT_BACKOFF_FACTOR,
                                0.5,
                            ));
                            let _ = service.db.with_writer(|repos| {
                                repos.fail_job(job.id, &format!("保存失敗: {error}"), &retry_at)
                            });
                        }
                    }
                }
            }
            Err(error) => {
                // ワーカー死亡・タイムアウト等: 再試行待ちへ (acceptance 5-2)
                emit(
                    "embedding:notice",
                    &format!("埋め込みワーカーエラー: {error}"),
                );
                for (job, _, _) in &embed_jobs {
                    let retry_at = rfc3339_after_now(backoff_delay_ms(
                        job.attempts,
                        gento_core::jobs::DEFAULT_BACKOFF_BASE_MS,
                        gento_core::jobs::DEFAULT_BACKOFF_FACTOR,
                        0.5,
                    ));
                    let _ = service
                        .db
                        .with_writer(|repos| repos.fail_job(job.id, &error, &retry_at));
                }
            }
        }
    }
    true
}

/// payload内のsource_hashを取り出す (sweep投入時の形式)。
fn payload_hash_of(job: &gento_db::JobRecord) -> String {
    #[derive(serde::Deserialize)]
    struct Payload {
        source_hash: Option<String>,
    }
    serde_json::from_str::<Payload>(&job.payload)
        .ok()
        .and_then(|p| p.source_hash)
        .unwrap_or_default()
}

// ===== Tauri command =====

fn service_of(state: &State<'_, AppState>) -> Result<Arc<EmbeddingService>, String> {
    state
        .embedding
        .clone()
        .ok_or_else(|| "埋め込みサービスが利用できません (DBが開けていません)".to_string())
}

#[tauri::command]
pub async fn embedding_status(state: State<'_, AppState>) -> Result<EmbeddingStatusDto, String> {
    let service = service_of(&state)?;
    tauri::async_runtime::spawn_blocking(move || service.status())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn embedding_enable(
    state: State<'_, AppState>,
    model: Option<String>,
) -> Result<EmbeddingStatusDto, String> {
    let service = service_of(&state)?;
    // WP-12: 有効化後に索引のロード/構築を予約 (バックグラウンド)
    let vector_index = state.vector_index.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let status = service.enable(model.as_deref())?;
        if let Some(vector_index) = vector_index {
            vector_index.ensure_ready();
        }
        Ok(status)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn embedding_disable(state: State<'_, AppState>) -> Result<EmbeddingStatusDto, String> {
    let service = service_of(&state)?;
    tauri::async_runtime::spawn_blocking(move || service.disable())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn embedding_model_delete(
    state: State<'_, AppState>,
    model: String,
) -> Result<EmbeddingStatusDto, String> {
    let service = service_of(&state)?;
    // WP-12: モデルのUSearch索引も破棄する (ベクトル正本と同時に)
    if let Some(vector_index) = state.vector_index.clone() {
        let model_for_index = model.clone();
        tauri::async_runtime::spawn_blocking(move || {
            vector_index.delete_index_for_model(&model_for_index)
        })
        .await
        .map_err(|e| e.to_string())?;
    }
    tauri::async_runtime::spawn_blocking(move || service.delete_model(&model))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn embedding_similar_debug(
    state: State<'_, AppState>,
    asset_id: Option<String>,
    query_text: Option<String>,
    k: Option<u32>,
) -> Result<Vec<SimilarItemDto>, String> {
    let service = service_of(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        let asset = asset_id
            .map(|value| value.parse::<i64>())
            .transpose()
            .map_err(|_| "asset_idが整数として解析できません".to_string())?;
        service.similar_debug(asset, query_text.as_deref(), k.unwrap_or(10))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    // ===== DTO直列化形状 (FIX-AI-DTO教訓: フィールド名契約の固定) =====

    #[test]
    fn status_dto_serializes_camel_case() {
        let dto = EmbeddingStatusDto {
            enabled: false,
            model: "ruri-v3-70m".to_string(),
            models_dir: "/models".to_string(),
            packages: vec![ModelPackageStateDto {
                model_id: "ruri-v3-70m".to_string(),
                state: "verified".to_string(),
                detail: "検証済み".to_string(),
                package_dir: "/models/ruri-v3-70m".to_string(),
                embedding_dim: 384,
                license: "Apache-2.0".to_string(),
            }],
            worker: WorkerPhaseDto {
                phase: WorkerPhase::NotStarted,
                restart_count: 0,
                last_error: None,
            },
            loaded_model: None,
            embedded_count: 0,
            total_count: 0,
            pending_jobs: 0,
            selfcheck_margin: None,
            warning: None,
        };
        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["enabled"], false);
        assert_eq!(json["model"], "ruri-v3-70m");
        assert_eq!(json["modelsDir"], "/models");
        assert_eq!(json["packages"][0]["modelId"], "ruri-v3-70m");
        assert_eq!(json["packages"][0]["embeddingDim"], 384);
        assert_eq!(json["packages"][0]["packageDir"], "/models/ruri-v3-70m");
        assert_eq!(json["worker"]["phase"], "not_started");
        assert_eq!(json["worker"]["restartCount"], 0);
        assert_eq!(json["loadedModel"], serde_json::Value::Null);
        assert_eq!(json["embeddedCount"], 0);
        assert_eq!(json["totalCount"], 0);
        assert_eq!(json["pendingJobs"], 0);
        assert_eq!(json["selfcheckMargin"], serde_json::Value::Null);
        assert_eq!(json["warning"], serde_json::Value::Null);
        // フロントが読むフィールドがsnake_caseで漏れていない
        for banned in [
            "models_dir",
            "embedded_count",
            "total_count",
            "pending_jobs",
            "selfcheck_margin",
            "loaded_model",
        ] {
            assert!(
                json.get(banned).is_none(),
                "snake_caseフィールド {banned} が混入"
            );
        }
    }

    #[test]
    fn similar_item_dto_serializes_camel_case() {
        let dto = SimilarItemDto {
            asset_id: "42".to_string(),
            score: 0.75,
        };
        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["assetId"], "42");
        assert_eq!(json["score"], 0.75);
        assert!(json.get("asset_id").is_none());
    }

    #[test]
    fn worker_phase_names() {
        assert_eq!(
            serde_json::to_value(WorkerPhase::NotStarted).unwrap(),
            serde_json::json!("not_started")
        );
        assert_eq!(
            serde_json::to_value(WorkerPhase::Running).unwrap(),
            serde_json::json!("running")
        );
        assert_eq!(
            serde_json::to_value(WorkerPhase::Restarting).unwrap(),
            serde_json::json!("restarting")
        );
        assert_eq!(
            serde_json::to_value(WorkerPhase::Crashed).unwrap(),
            serde_json::json!("crashed")
        );
    }

    #[test]
    fn request_id_injection_all_variants() {
        let cases = vec![
            gento_embedding::WorkerRequest::Load {
                id: 0,
                model: "m".into(),
                models_dir: "/d".into(),
                threads: None,
            },
            gento_embedding::WorkerRequest::Unload { id: 0 },
            gento_embedding::WorkerRequest::EmbedQuery {
                id: 0,
                text: "t".into(),
            },
            gento_embedding::WorkerRequest::EmbedDocuments {
                id: 0,
                texts: vec!["a".into()],
            },
            gento_embedding::WorkerRequest::Status { id: 0 },
            gento_embedding::WorkerRequest::Shutdown { id: 0 },
        ];
        for request in cases {
            let with_id = with_request_id(request, 77);
            let json = serde_json::to_value(&with_id).unwrap();
            assert_eq!(json["id"], 77);
        }
    }

    #[test]
    fn payload_hash_of_extracts_hash() {
        let job = gento_db::JobRecord {
            id: 1,
            uuid: "u".into(),
            job_type: "embedding".into(),
            target_type: "asset".into(),
            target_id: 5,
            status: "running".into(),
            priority: 4,
            attempts: 1,
            max_attempts: 3,
            payload: r#"{"source_hash":"abc123"}"#.into(),
            error: None,
            scheduled_at: None,
            started_at: None,
            finished_at: None,
            created_at: "c".into(),
            updated_at: "u".into(),
        };
        assert_eq!(payload_hash_of(&job), "abc123");
        let mut broken = job;
        broken.payload = "{}".into();
        assert_eq!(payload_hash_of(&broken), "");
    }

    #[test]
    fn blake3_hex_is_stable_and_hex() {
        let a = blake3_hex("テスト文書");
        let b = blake3_hex("テスト文書");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, blake3_hex("別の文書"));
    }

    #[test]
    fn resolve_models_dir_for_app_flag_and_env() {
        let app_dirs = gento_core::AppDirs {
            data_dir: std::path::PathBuf::from("/xdg/data"),
            cache_dir: std::path::PathBuf::from("/xdg/cache"),
            log_dir: std::path::PathBuf::from("/xdg/log"),
        };
        // --models-dir引数が最優先
        let args = vec!["--models-dir".to_string(), "/local/models".to_string()];
        assert_eq!(
            resolve_models_dir_for_app(&args, &app_dirs),
            PathBuf::from("/local/models")
        );
        // 無指定なら既定 data_dir/models
        assert_eq!(
            resolve_models_dir_for_app(&[], &app_dirs),
            PathBuf::from("/xdg/data/models")
        );
    }

    #[test]
    fn model_revision_of_known_models() {
        assert_eq!(
            model_revision_of("ruri-v3-70m"),
            "07a8b0aba47d29d2ca21f89b915c1efe2c23d1cc"
        );
        assert_eq!(model_revision_of("unknown-model"), "unknown");
    }

    // ===== 差分掃討スイープ (実DB) =====

    use std::collections::HashSet;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "gento-tauri-embed-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis()
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

    /// スイープ経路でワーカーspawnが起きないことの担保 (spawn試行=
    /// 設計違反で即パニック)。
    fn panicking_worker_factory() -> WorkerCommandFactory {
        Box::new(|_path: &PathBuf| -> Command {
            panic!("スイープはワーカーを起動しない (spawn試行 = 設計違反)");
        })
    }

    /// presentなFileを持つassetを一括投入 (search.rs性能テストと同じ
    /// 合成行SQL。空でないdisplay_nameを持つため全員が空でない検索文書を
    /// 持つ)。uuid/hash/パスは `start` からの一意連番。昇順id列を返す。
    fn seed_present_assets(db: &gento_db::Db, start: i64, count: i64) -> Vec<i64> {
        use gento_core::repo::LibraryRootRepo;
        db.with_writer(|repos| {
            let root = repos.list_roots()?.into_iter().next().unwrap_or_else(|| {
                repos
                    .add_root(std::path::Path::new("/tmp/gento-sweep-test"))
                    .unwrap()
            });
            let conn = repos.conn();
            conn.execute_batch("BEGIN").unwrap();
            let ts = "2026-10-08T00:00:00.000Z";
            let mut ids = Vec::with_capacity(count as usize);
            for i in start..start + count {
                conn.execute(
                    "INSERT INTO assets (uuid, content_hash, hash_algorithm, byte_size, \
                     mime_type, created_at, updated_at) \
                     VALUES (?1, ?2, 'blake3', 100, 'image/png', ?3, ?3)",
                    (
                        format!("00000000-0000-7000-8000-{i:012}"),
                        format!("blake3:sweep{i:08x}"),
                        ts,
                    ),
                )
                .unwrap();
                let asset_id = conn.last_insert_rowid();
                conn.execute(
                    "INSERT INTO files (uuid, root_id, asset_id, rel_path, path_encoding, \
                     display_name, extension, byte_size, mtime_ms, last_seen_at, \
                     created_at, updated_at) \
                     VALUES (?1, ?2, ?3, CAST(?4 AS BLOB), 'utf8', ?5, 'png', 100, 1, ?6, ?6, ?6)",
                    (
                        format!("00000000-0000-7000-9000-{i:012}"),
                        root.id,
                        asset_id,
                        format!("dir/sweep{i:06}.png"),
                        format!("sweep{i:06}.png"),
                        ts,
                    ),
                )
                .unwrap();
                ids.push(asset_id);
            }
            conn.execute_batch("COMMIT").unwrap();
            Ok(ids)
        })
        .unwrap()
    }

    /// queued埋め込みジョブをすべて消化模擬 (claim → 埋め込みupsert → 完了。
    /// run_one_batchの成功経路と同じmeta保存)。消化した (asset_id, priority)
    /// を `out` へ追記し、消化件数を返す。
    fn drain_embedding_jobs(service: &EmbeddingService, out: &mut Vec<(i64, i64)>) -> usize {
        let model = service.selected_model();
        let revision = model_revision_of(&model);
        let mut count = 0usize;
        loop {
            let now = rfc3339_after_now(0);
            let claimed = service
                .db
                .with_writer(|repos| repos.claim_next_job_of_type(&now, JOB_TYPE_EMBEDDING))
                .unwrap_or(None);
            let Some(job) = claimed else { break };
            let hash = payload_hash_of(&job);
            let vector = vec![0.25f32; 4];
            service
                .db
                .with_writer(|repos| {
                    repos.embedding_upsert(
                        job.target_id,
                        &model,
                        &revision,
                        vector.len(),
                        "f32",
                        &hash,
                        &vector,
                    )
                })
                .unwrap();
            service
                .db
                .with_writer(|repos| repos.complete_job(job.id))
                .unwrap();
            out.push((job.target_id, job.priority));
            count += 1;
        }
        count
    }

    /// assetをpresentでなくす (ファイル消失相当: scan_state='missing')。
    fn mark_assets_missing(db: &gento_db::Db, ids: &[i64]) {
        db.with_writer(|repos| {
            for id in ids {
                repos
                    .conn()
                    .execute(
                        "UPDATE files SET scan_state = 'missing' WHERE asset_id = ?1",
                        (id,),
                    )
                    .unwrap();
            }
            Ok(())
        })
        .unwrap();
    }

    /// 回帰: SWEEP_PAGE上限を超えるライブラリで定期スイープを繰り返すと
    /// 全present assetがいずれかのスイープでエンキューされる。常時先頭から
    /// 走査する実装だと id順末尾が毎回上限の裏に隠れて永久に検査されない。
    #[test]
    fn sweep_enqueues_all_present_assets_across_repeated_sweeps() {
        let tmp = TempDir::new("sweep-all");
        let db = Arc::new(gento_db::Db::open(tmp.0.join("db.sqlite3")).unwrap());
        let all_ids = seed_present_assets(&db, 0, 2100);
        let notices = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
        let recorded = Arc::clone(&notices);
        let service = EmbeddingService::start_with(
            Arc::clone(&db),
            tmp.0.join("models"),
            Arc::new(move |event: &str, payload: &str| {
                recorded
                    .lock()
                    .unwrap()
                    .push((event.to_string(), payload.to_string()));
            }),
            panicking_worker_factory(),
        );
        // 1スイープの作業量上限: SWEEP_PAGE(2000)を256/pageで消費すると
        // 8ページ=2048件で打ち切り (ページ繰上げ契約)
        let first_sweep_cap =
            SWEEP_PAGE.div_ceil(SWEEP_READ_PAGE as usize) * SWEEP_READ_PAGE as usize;
        assert_eq!(first_sweep_cap, 2048);
        let enqueued1 = service.sweep_enqueues_pending(SWEEP_PAGE);
        assert_eq!(
            enqueued1, first_sweep_cap,
            "初回スイープは上限+繰上げの2048件で打ち切る"
        );
        let mut round1 = Vec::new();
        assert_eq!(drain_embedding_jobs(&service, &mut round1), first_sweep_cap);
        assert!(
            round1.iter().all(|(_, p)| *p == PRIORITY_INITIAL),
            "モデル埋め込み0件の初回はINITIAL優先度"
        );
        // 定期スイープ (SWEEP_INTERVAL毎の想定) を繰り返して残りを拾う
        let mut seen: HashSet<i64> = round1.iter().map(|(a, _)| *a).collect();
        let mut later = Vec::new();
        let mut zero_rounds = 0;
        for _ in 0..8 {
            let enqueued = service.sweep_enqueues_pending(SWEEP_PAGE);
            let drained = drain_embedding_jobs(&service, &mut later);
            if enqueued == 0 && drained == 0 {
                zero_rounds += 1;
                if zero_rounds >= 2 {
                    break; // 2連続で新規なし = 定常状態
                }
            } else {
                zero_rounds = 0;
            }
        }
        seen.extend(later.iter().map(|(a, _)| *a));
        let missing: Vec<i64> = all_ids
            .iter()
            .copied()
            .filter(|id| !seen.contains(id))
            .collect();
        assert!(
            missing.is_empty(),
            "スイープで拾えなかったasset {missing:?} ({}/{}件しかエンキューされていない)",
            seen.len(),
            all_ids.len()
        );
        assert!(
            later.iter().all(|(_, p)| *p == PRIORITY_NEW_ASSET),
            "埋め込み済みが存在する状態の差分はNEW_ASSET優先度"
        );
        assert!(
            notices.lock().unwrap().is_empty(),
            "スイープ経路で通知イベントは出ない"
        );
        service.shutdown();
    }

    /// 終端巻き戻し: テーブル終端に達したスイープは次回先頭から再検査する。
    /// 2周目は末尾追加の新規assetに加え、既存assetの検索文書変化 (ファイル名
    /// 変更) も拾う — 巻き戻さない前半は二度と差分検査されない。
    #[test]
    fn sweep_rewinds_at_table_end_and_reexamines_earlier_assets() {
        let tmp = TempDir::new("sweep-rewind");
        let db = Arc::new(gento_db::Db::open(tmp.0.join("db.sqlite3")).unwrap());
        let base = seed_present_assets(&db, 0, 300);
        let service = EmbeddingService::start_with(
            Arc::clone(&db),
            tmp.0.join("models"),
            Arc::new(|_: &str, _: &str| {}),
            panicking_worker_factory(),
        );
        // 300件 < 上限 → 1回のスイープで終端到達 (次回は先頭から)
        assert_eq!(service.sweep_enqueues_pending(SWEEP_PAGE), 300);
        let mut round1 = Vec::new();
        assert_eq!(drain_embedding_jobs(&service, &mut round1), 300);
        // 末尾に新規3件 + 既存の先頭付近1件を変更 (検索文書のhash変化)
        let mut expected = seed_present_assets(&db, 300, 3);
        let changed = base[1];
        db.with_writer(|repos| {
            repos
                .conn()
                .execute(
                    "UPDATE files SET display_name = 'renamed.png' WHERE asset_id = ?1",
                    (changed,),
                )
                .map_err(|e| gento_core::repo::RepoError::Db(e.to_string()))?;
            Ok(())
        })
        .unwrap();
        expected.push(changed);
        expected.sort_unstable();
        // 2周目: 新規3件 + 変更1件がエンキューされる
        let enqueued2 = service.sweep_enqueues_pending(SWEEP_PAGE);
        assert_eq!(enqueued2, expected.len());
        let mut round2 = Vec::new();
        drain_embedding_jobs(&service, &mut round2);
        let mut got: Vec<i64> = round2.iter().map(|(a, _)| *a).collect();
        got.sort_unstable();
        assert_eq!(got, expected);
        // 3周目: 全件unchanged → 追加エンキューなし
        assert_eq!(service.sweep_enqueues_pending(SWEEP_PAGE), 0);
        service.shutdown();
    }

    /// カーソル継続は削除済み (present喪失) assetの混在で破綻しない:
    /// keyset (asset_id > cursor) 走査なので欠落は読み飛ばされる。
    #[test]
    fn sweep_continues_across_missing_assets_beyond_cursor() {
        let tmp = TempDir::new("sweep-missing");
        let db = Arc::new(gento_db::Db::open(tmp.0.join("db.sqlite3")).unwrap());
        let all = seed_present_assets(&db, 0, 600);
        let service = EmbeddingService::start_with(
            Arc::clone(&db),
            tmp.0.join("models"),
            Arc::new(|_: &str, _: &str| {}),
            panicking_worker_factory(),
        );
        // limit=300 → 2ページ (512件) で打ち切り、カーソルは512件目に残る
        let first_cap = 300usize.div_ceil(SWEEP_READ_PAGE as usize) * SWEEP_READ_PAGE as usize;
        assert_eq!(first_cap, 512);
        assert_eq!(service.sweep_enqueues_pending(300), first_cap);
        let mut round1 = Vec::new();
        assert_eq!(drain_embedding_jobs(&service, &mut round1), first_cap);
        // カーソル位置を跨ぐように present喪失 (id 450..560相当)
        mark_assets_missing(&db, &all[449..560]);
        // 継続スイープ: カーソルより後の生存asset (末尾40件) を欠落なく検査
        let survivors: Vec<i64> = all[560..600].to_vec();
        let enqueued2 = service.sweep_enqueues_pending(300);
        assert_eq!(enqueued2, survivors.len());
        let mut round2 = Vec::new();
        drain_embedding_jobs(&service, &mut round2);
        let mut got: Vec<i64> = round2.iter().map(|(a, _)| *a).collect();
        got.sort_unstable();
        assert_eq!(got, survivors);
        // 全件消失しても破綻しない (空ページ → 巻き戻し → 終了)
        mark_assets_missing(&db, &all);
        assert_eq!(service.sweep_enqueues_pending(300), 0);
        service.shutdown();
    }
}
