//! AIプロファイル・永続ジョブのIPC (MVP WP-10)。
//!
//! - command群は [`AppState::ai`] (`AiService`) への薄いアダプター。
//! - [`AiService`] はバックグラウンドワーカー (std::thread。library.rsの
//!   ワーカー規約に同じ) でjobs表をポーリング (500ms) し、起床条件は
//!   「エンキュー時・resume時・ポーリング期限」。キューの一時停止は
//!   メモリフラグ (永続化しない — 再起動で実行再開が仕様§11.2の既定)。
//! - ジョブ状態イベント `ai:jobs` は500msバッチでemitterスレッドから
//!   送出される (高頻度イベント抑制)。
//! - **APIキーはここでのみSecretStoreを経由する** (DB・ログへ書かない)。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gento_ai::client::{
    test_connection, test_structured, test_text, test_vision, AiClient, AiEndpoint,
};
use gento_ai::pipeline::{run_caption_tags_job, AiRunOutcome, PipelineOptions};
use gento_ai::prompt::ProfileExtra;
use gento_ai::{ApiKey, SecretStore};
use gento_core::jobs::{backoff_delay_ms, DEFAULT_BACKOFF_BASE_MS, DEFAULT_BACKOFF_FACTOR};
use gento_db::{
    rfc3339_after_now, AiProfileRecord, Db, JobRecord, JOB_TYPE_AI_CAPTION_TAGS,
    JOB_TYPE_AI_REPARSE,
};
use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Emitter, State};

use crate::catalog::AppState;

/// ワーカーのポーリング間隔 (起動条件: エンキュー通知・resume・期限)。
const POLL_INTERVAL: Duration = Duration::from_millis(500);
/// イベントバッチ窓口 (ai:jobsの高頻度抑制)。
const EVENT_BATCH_WINDOW: Duration = Duration::from_millis(500);
/// ワーカースレッド数 (プロファイル同時実行の上限は各プロファイルの
/// concurrency設定でジョブ側に課す。全体ワーカーは固定小数)。
const WORKER_THREADS: usize = 2;
/// 候補覗き見件数 (飽和プロファイル判定のための余白)。
const ELIGIBLE_PEEK: u32 = 16;
/// ジョブ優先度 (MVP固定値。UI操作由来=5。将来は操作種で変える)。
const JOB_PRIORITY: i64 = 5;

// ===== DTO =====

/// プロファイルDTO (`ai_profile_list` / 保存応答)。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiProfileDto {
    pub id: String,
    pub uuid: String,
    pub display_name: String,
    pub base_url: String,
    pub model: String,
    pub vision_capable: bool,
    pub enabled: bool,
    pub timeout_ms: i64,
    pub concurrency: i64,
    /// プロファイル追加設定 (パース済みJSON)。
    pub extra: serde_json::Value,
    /// APIキーが登録済みか (参照の有無のみ。本体は返さない)。
    pub has_api_key: bool,
    /// APIキーの保存先 ("keyring" | "file" | null)。
    pub api_key_store: Option<String>,
}

/// ジョブDTO (`ai_jobs_list`)。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiJobDto {
    pub id: String,
    pub job_type: String,
    pub target_id: String,
    pub status: String,
    pub priority: i64,
    pub attempts: u32,
    pub max_attempts: u32,
    pub error: Option<String>,
    /// retry_waitingの再試行予定時刻。
    pub scheduled_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// AI実行履歴DTO (`ai_run_history`)。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiRunDto {
    pub id: String,
    pub profile_id: Option<String>,
    pub operation: String,
    pub status: String,
    pub prompt_version: i64,
    pub request_summary: Option<serde_json::Value>,
    pub error: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
}

/// `ai:jobs` イベントペイロード (500msバッチ)。
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AiJobsEventPayload {
    /// 前バッチ以降に変化のあったジョブ。
    pub changes: Vec<AiJobDto>,
    /// 状態ごとの件数 (バッチ時点の全体)。
    pub counts: HashMap<String, u64>,
    pub paused: bool,
}

/// イベント送出境界 (Tauri emit / テスト記録の差し替え)。
type EventSink = dyn Fn(&AiJobsEventPayload) + Send + Sync;

pub struct AiService {
    db: Arc<Db>,
    secrets: SecretStore,
    paused: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    /// プロファイル別の実行中件数 (concurrency制限のセマフォ。
    /// 実体はワーカーが参照 — ここはライフサイクル保持)。
    #[allow(dead_code)]
    in_flight: Arc<Mutex<HashMap<i64, usize>>>,
    /// ワーカー起床通知 (エンキュー・resume時)。
    wake_tx: Vec<SyncSender<()>>,
    workers: Vec<std::thread::JoinHandle<()>>,
    /// バッチemitterスレッド (Dropでjoin)。
    emitter: Option<std::thread::JoinHandle<()>>,
}

impl AiService {
    /// サービスを構築しワーカーを起動する (起動時回復を含む)。
    /// `emit` は `ai:jobs` イベントの送出境界 (Tauri emit / テスト記録)。
    pub fn start(db: Arc<Db>, data_dir: PathBuf, emit: Arc<EventSink>) -> Arc<Self> {
        let recovered = db
            .with_writer(|repos| repos.recover_running_jobs())
            .unwrap_or(0);
        if recovered > 0 {
            tracing::info!(count = recovered, "recovered interrupted AI jobs to queued");
        }
        let paused = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::new(AtomicBool::new(false));
        let in_flight = Arc::new(Mutex::new(HashMap::new()));

        // イベントバッチスレッド (チャネル経由の変更を500ms窓でまとめる)
        let (tx, rx): (SyncSender<AiJobDto>, Receiver<AiJobDto>) =
            std::sync::mpsc::sync_channel(256);
        let emitter = {
            let db = Arc::clone(&db);
            let paused = Arc::clone(&paused);
            let emit = Arc::clone(&emit);
            std::thread::Builder::new()
                .name("ai-jobs-emitter".into())
                .spawn(move || loop {
                    let first = match rx.recv_timeout(POLL_INTERVAL) {
                        Ok(change) => change,
                        // タイムアウト: 送るものが無い (継続)。
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                        // 全送信側drop (サービス停止) → 終了
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    let mut changes = vec![first];
                    // バッチ窓: 最初の変更から500ms以内に到着した分をまとめる
                    let deadline = Instant::now() + EVENT_BATCH_WINDOW;
                    while Instant::now() < deadline {
                        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                            Ok(change) => changes.push(change),
                            Err(_) => break,
                        }
                    }
                    let counts: HashMap<String, u64> = db
                        .with_reader(|repos| repos.job_status_counts())
                        .unwrap_or_default()
                        .into_iter()
                        .collect();
                    emit(&AiJobsEventPayload {
                        changes,
                        counts,
                        paused: paused.load(Ordering::SeqCst),
                    });
                })
                .expect("spawn ai-jobs-emitter")
        };

        // ワーカースレッド
        let mut workers = Vec::new();
        let mut wake_tx = Vec::new();
        for index in 0..WORKER_THREADS {
            let (wake, wake_rx) = std::sync::mpsc::sync_channel::<()>(8);
            let db = Arc::clone(&db);
            let worker_data_dir = data_dir.clone();
            let paused = Arc::clone(&paused);
            let shutdown = Arc::clone(&shutdown);
            let in_flight = Arc::clone(&in_flight);
            let change_tx = tx.clone();
            let handle = std::thread::Builder::new()
                .name(format!("ai-worker-{index}"))
                .spawn(move || {
                    worker_loop(
                        &db,
                        &worker_data_dir,
                        &paused,
                        &shutdown,
                        &in_flight,
                        &wake_rx,
                        &change_tx,
                    );
                })
                .expect("spawn ai worker");
            workers.push(handle);
            wake_tx.push(wake);
        }
        Arc::new(AiService {
            db,
            secrets: SecretStore::new(&data_dir),
            paused,
            shutdown,
            in_flight,
            wake_tx,
            workers,
            emitter: Some(emitter),
        })
    }

    /// 全ワーカーを起床させる (エンキュー・resume後)。
    pub fn wake_all(&self) {
        for tx in &self.wake_tx {
            let _ = tx.try_send(());
        }
    }

    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::SeqCst);
        if !paused {
            self.wake_all();
        }
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    /// Graceful停止 (Exit時)。
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        self.wake_all();
        tracing::info!("shutting down AI job service");
    }

    pub fn secret_store(&self) -> &SecretStore {
        &self.secrets
    }

    pub fn db(&self) -> &Arc<Db> {
        &self.db
    }
}

impl Drop for AiService {
    fn drop(&mut self) {
        self.shutdown();
        for handle in self.workers.drain(..) {
            let _ = handle.join();
        }
        // ワーカーが落りると変更チャネルの送信側が消え、emitterは
        // Disconnectedで自ら終了する (joinで確定待ち)。
        if let Some(emitter) = self.emitter.take() {
            let _ = emitter.join();
        }
    }
}

// ===== ワーカー =====

/// ワーカーのメインループ。`run_one_job` をテストから直接呼べるように
/// 分離している (決定的ドライブ)。
fn worker_loop(
    db: &Arc<Db>,
    data_dir: &std::path::Path,
    paused: &AtomicBool,
    shutdown: &AtomicBool,
    in_flight: &Mutex<HashMap<i64, usize>>,
    wake_rx: &Receiver<()>,
    change_tx: &SyncSender<AiJobDto>,
) {
    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        if paused.load(Ordering::SeqCst) {
            let _ = wake_rx.recv_timeout(Duration::from_millis(1000));
            continue;
        }
        let ran = run_one_job(db, data_dir, paused, in_flight, change_tx);
        if !ran {
            // ジョブが無い: 起床通知 or ポーリング間隔まで待つ
            let _ = wake_rx.recv_timeout(POLL_INTERVAL);
        }
    }
}

/// 1ジョブを実行する (候補選択→claim→実行→確定)。
/// 戻り値 = ジョブを実行したか (false = 対象なし)。
/// テストから直接呼んで決定的にドライブできる。
/// `paused` はclaim直前にも再検査する (worker_loopの検査通過後にpauseが
/// 設定された場合も新規claimを避ける — pauseは協調的フラグのため境界は
/// best-effortだが窓を最小化する)。
fn run_one_job(
    db: &Arc<Db>,
    data_dir: &std::path::Path,
    paused: &AtomicBool,
    in_flight: &Mutex<HashMap<i64, usize>>,
    change_tx: &SyncSender<AiJobDto>,
) -> bool {
    let now = rfc3339_after_now(0);
    let candidates: Vec<JobRecord> = db
        .with_reader(|repos| repos.list_eligible_jobs(&now, ELIGIBLE_PEEK))
        .unwrap_or_default();
    if candidates.is_empty() {
        return false;
    }
    // プロファイル飽和チェック・claim・スロット予約を同一ロック内で行う
    // (FG-WP10 blocker 3: チェックと予約の間に別workerが同じプロファイルの
    // ジョブをclaimする競合を構造的に排除する)。
    let claimed_job: Option<(JobRecord, Option<AiProfileRecord>)> = {
        let mut saturated = in_flight.lock().unwrap_or_else(|p| p.into_inner());
        // 候補読み取り後にpauseされた場合は新規claimしない
        if paused.load(Ordering::SeqCst) {
            return false;
        }
        let chosen = candidates.iter().find_map(|job| {
            let profile = profile_of_job(db, job);
            let has_capacity = match &profile {
                Some(profile) => {
                    let running = saturated.get(&profile.id).copied().unwrap_or(0);
                    running < profile.concurrency.max(1) as usize
                }
                // プロファイル未解決のジョブは飽和計算外 (即failするため)
                None => true,
            };
            has_capacity.then(|| (job.clone(), profile))
        });
        let Some((job, profile)) = chosen else {
            return false; // 全候補が飽和 — 次のポーリングで再評価
        };
        let claimed = db
            .with_writer(|repos| repos.claim_job_by_id(job.id, &now))
            .unwrap_or(None);
        match claimed {
            Some(claimed) => {
                // 予約成功でのみスロットを確保 (claim敗退時は何もしない)
                if let Some(profile) = &profile {
                    *saturated.entry(profile.id).or_insert(0) += 1;
                }
                Some((claimed, profile))
            }
            // 他ワーカーに取られた → ロック解放後に即再ループ
            None => return true,
        }
    };
    let (claimed, profile) = claimed_job.expect("matched arm always sets");
    let _ = change_tx.try_send(job_dto(&claimed));

    let outcome_result = execute_claimed_job(db, data_dir, &claimed, profile.as_ref());
    if let Some(profile) = &profile {
        let mut counts = in_flight.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(count) = counts.get_mut(&profile.id) {
            *count = count.saturating_sub(1);
        }
    }
    match outcome_result {
        Ok(_) => {
            let _ = db.with_writer(|repos| repos.complete_job(claimed.id));
        }
        Err((retryable, error)) => {
            if retryable {
                // network/5xx/timeout: backoff付きで再試行待機へ
                let jitter = pseudo_jitter();
                let delay = backoff_delay_ms(
                    claimed.attempts,
                    DEFAULT_BACKOFF_BASE_MS,
                    DEFAULT_BACKOFF_FACTOR,
                    jitter,
                );
                let retry_at = rfc3339_after_now(delay);
                let _ = db.with_writer(|repos| repos.fail_job(claimed.id, &error, &retry_at));
            } else {
                // 4xx/認証/パース失敗等: 再送しても同じ結果 — 即座にterminal
                // failed (retry_waitingに入れない。画像再開示の反復を防止)
                let _ = db.with_writer(|repos| repos.fail_job_permanent(claimed.id, &error));
            }
        }
    }
    let after = db
        .with_reader(|repos| {
            repos
                .list_jobs(64)
                .map(|jobs| jobs.into_iter().find(|j| j.id == claimed.id))
        })
        .unwrap_or(None);
    if let Some(job) = after {
        let _ = change_tx.try_send(job_dto(&job));
    }
    true
}

/// claim済みジョブの実行 (種別分岐)。
fn execute_claimed_job(
    db: &Arc<Db>,
    data_dir: &std::path::Path,
    claimed: &JobRecord,
    profile: Option<&AiProfileRecord>,
) -> Result<String, (bool, String)> {
    let secrets = SecretStore::new(data_dir);
    match (claimed.job_type.as_str(), profile) {
        (JOB_TYPE_AI_CAPTION_TAGS, Some(profile)) => {
            // 鍵参照があるのに取得失敗 (keyring/fileエラー・ファイル消失) は
            // フェイルクローズ: 未認証での送信はしない (FG-WP10 blocker 5)
            let api_key = match profile.api_key_ref.as_deref() {
                None => None,
                Some(key_ref) => match secrets.load(key_ref) {
                    Some(key) => Some(key),
                    None => {
                        return Err((
                            false,
                            "資格情報の取得に失敗しました (プロファイルのAPIキーが\
                             読み取れません。保存し直してください)"
                                .to_string(),
                        ));
                    }
                },
            };
            let result =
                run_caption_tags_job(db, claimed, profile, api_key, &PipelineOptions::default());
            match result {
                AiRunOutcome::Succeeded { .. } => Ok("succeeded".to_string()),
                AiRunOutcome::Failed {
                    error, retryable, ..
                } => Err((retryable, error)),
            }
        }
        (JOB_TYPE_AI_REPARSE, _) => run_reparse_for_asset(db, claimed.target_id),
        // プロファイル未解決 (削除済み等): 再試行しても解決しないため確定失敗
        _ => Err((
            false,
            "AIプロファイルが見つかりません (削除された可能性)".to_string(),
        )),
    }
}

/// プロファイル未満の擬似乱数ジッター [0,1) (追加依存を避け、時刻由来)。
fn pseudo_jitter() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    (nanos % 1_000) as f64 / 1_000.0
}

/// ジョブpayloadの `profile_id` からプロファイルを解決する。
/// 無ければ有効プロファイル (自動処理と同じ規則)。
fn profile_of_job(db: &Arc<Db>, job: &JobRecord) -> Option<AiProfileRecord> {
    let profile_id: Option<i64> = serde_json::from_str::<serde_json::Value>(&job.payload)
        .ok()
        .and_then(|v| v.get("profile_id").and_then(|p| p.as_i64()));
    db.with_reader(|repos| {
        Ok(match profile_id {
            Some(id) => repos.get_ai_profile(id)?,
            None => repos.first_enabled_ai_profile()?,
        })
    })
    .unwrap_or(None)
}

/// `ai_reparse` ジョブの実体: asset1件の埋め込みメタデータ再解析
/// (WP-9 run_metadata_reparse の1asset版。既存直接実行 (bulk) は互換で残す)。
fn run_reparse_for_asset(db: &Arc<Db>, asset_id: i64) -> Result<String, (bool, String)> {
    use gento_core::assetmeta::AssetMetadataRepo as _;
    use gento_core::library::GenerationToken;
    let source = db
        .with_reader(|repos| repos.representative_present_file(asset_id))
        .map_err(|e| (false, e.to_string()))?;
    let Some(source) = source else {
        return Err((false, "対象ファイルがありません".to_string()));
    };
    let token = GenerationToken::from_record(&source.file);
    let path = source.absolute_path();
    let parsed = gento_image::metadata::reparse_metadata_file(&path, &token)
        .map_err(|e| (false, format!("再解析に失敗しました: {e}")))?;
    db.with_writer(|repos| repos.save_asset_metadata(asset_id, &parsed))
        .map_err(|e| (false, e.to_string()))?;
    Ok("reparsed".to_string())
}

fn job_dto(job: &JobRecord) -> AiJobDto {
    AiJobDto {
        id: job.id.to_string(),
        job_type: job.job_type.clone(),
        target_id: job.target_id.to_string(),
        status: job.status.clone(),
        priority: job.priority,
        attempts: job.attempts,
        max_attempts: job.max_attempts,
        error: job.error.clone(),
        scheduled_at: job.scheduled_at.clone(),
        created_at: job.created_at.clone(),
        updated_at: job.updated_at.clone(),
    }
}

fn profile_dto(profile: &AiProfileRecord, key: Option<&ApiKey>) -> AiProfileDto {
    let store = profile
        .api_key_ref
        .as_deref()
        .and_then(gento_ai::StoredSecretRef::parse)
        .map(|r| r.kind.as_str().to_string());
    AiProfileDto {
        id: profile.id.to_string(),
        uuid: profile.uuid.clone(),
        display_name: profile.display_name.clone(),
        base_url: profile.base_url.clone(),
        model: profile.model.clone(),
        vision_capable: profile.vision_capable,
        enabled: profile.enabled,
        timeout_ms: profile.timeout_ms,
        concurrency: profile.concurrency,
        extra: ProfileExtra::parse(profile.extra_json.as_deref())
            .to_json_string()
            .parse()
            .unwrap_or(serde_json::json!({})),
        has_api_key: key.is_some() || store.is_some(),
        api_key_store: store,
    }
}

// ===== DB共通ヘルパ (テストからも使用) =====

pub(crate) fn ai_service_of(state: &AppState) -> Result<Arc<AiService>, String> {
    state
        .ai
        .as_ref()
        .cloned()
        .ok_or_else(|| "AI機能が利用できません (データベース未接続)".to_string())
}

fn parse_i64(value: &str, label: &str) -> Result<i64, String> {
    value
        .parse()
        .map_err(|_| format!("無効な{label}です: {value}"))
}

// ===== Commands (プロファイル) =====

#[tauri::command]
pub async fn ai_profile_list(state: State<'_, AppState>) -> Result<Vec<AiProfileDto>, String> {
    let service = ai_service_of(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        let profiles = service
            .db()
            .with_reader(|repos| repos.list_ai_profiles())
            .map_err(|e| format!("プロファイル一覧の取得に失敗しました: {e}"))?;
        Ok(profiles
            .iter()
            .map(|p| {
                let key = p
                    .api_key_ref
                    .as_deref()
                    .and_then(|r| service.secret_store().load(r));
                profile_dto(p, key.as_ref())
            })
            .collect())
    })
    .await
    .map_err(|e| format!("プロファイル一覧の取得に失敗しました: {e}"))?
}

/// 保存 (upsert)。`api_key`: None=変更なし / Some("")=削除 / Some(値)=保存。
#[tauri::command]
pub async fn ai_profile_save(
    state: State<'_, AppState>,
    profile: serde_json::Value,
    api_key: Option<String>,
) -> Result<AiProfileDto, String> {
    let service = ai_service_of(&state)?;
    tauri::async_runtime::spawn_blocking(move || save_profile_impl(&service, &profile, api_key))
        .await
        .map_err(|e| format!("プロファイルの保存に失敗しました: {e}"))?
}

fn save_profile_impl(
    service: &AiService,
    input: &serde_json::Value,
    api_key: Option<String>,
) -> Result<AiProfileDto, String> {
    let display_name = input
        .get("displayName")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if display_name.is_empty() {
        return Err("表示名を入力してください".to_string());
    }
    let base_url = input
        .get("baseUrl")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .trim_end_matches('/')
        .to_string();
    if base_url.is_empty() {
        return Err("Base URLを入力してください".to_string());
    }
    let model = input
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if model.is_empty() {
        return Err("モデル名を入力してください".to_string());
    }
    let mut extra = match input.get("extra") {
        Some(value) if value.is_object() => {
            let parsed: ProfileExtra = serde_json::from_value(value.clone())
                .map_err(|e| format!("追加設定が不正です: {e}"))?;
            // 送信画像サイズは範囲外をIPCで拒否 (旧「0=縮小なし」・2048超は
            // 保存させない。保存済み値の読み込み側クランプはProfileExtra::parse)
            if !(gento_ai::imaging::MIN_IMAGE_EDGE..=gento_ai::imaging::MAX_IMAGE_EDGE)
                .contains(&parsed.max_image_edge)
            {
                return Err(format!(
                    "送信画像の最大辺は{}〜{}pxの範囲で指定してください (送信は最大{}pxまで)",
                    gento_ai::imaging::MIN_IMAGE_EDGE,
                    gento_ai::imaging::MAX_IMAGE_EDGE,
                    gento_ai::imaging::MAX_IMAGE_EDGE
                ));
            }
            parsed
        }
        _ => ProfileExtra::default(),
    };
    // extraの過去値を引き継ぐ (UIが省略時)
    let timeout_ms = input
        .get("timeoutMs")
        .and_then(|v| v.as_i64())
        .unwrap_or(60_000)
        .clamp(1_000, 600_000);
    let concurrency = input
        .get("concurrency")
        .and_then(|v| v.as_i64())
        .unwrap_or(2)
        .clamp(1, 8);
    let vision_capable = input
        .get("visionCapable")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let enabled = input
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let db = service.db();
    let existing: Option<AiProfileRecord> = match input.get("id").and_then(|v| v.as_str()) {
        Some(id_str) if !id_str.is_empty() => {
            let id = parse_i64(id_str, "プロファイルID")?;
            db.with_reader(|repos| repos.get_ai_profile(id))
                .map_err(|e| e.to_string())?
        }
        _ => None,
    };
    let record = match existing {
        Some(mut existing) => {
            // UIがextraを省略した場合は既存値を引き継ぐ
            if input.get("extra").is_none() {
                extra = ProfileExtra::parse(existing.extra_json.as_deref());
            }
            existing.display_name = display_name;
            existing.base_url = base_url;
            existing.model = model;
            existing.vision_capable = vision_capable;
            existing.enabled = enabled;
            existing.timeout_ms = timeout_ms;
            existing.concurrency = concurrency;
            existing.extra_json = Some(extra.to_json_string());
            db.with_writer(|repos| repos.update_ai_profile_settings(&existing))
                .map_err(|e| e.to_string())?;
            existing
        }
        None => db
            .with_writer(|repos| {
                repos.insert_ai_profile(&gento_db::NewAiProfile {
                    display_name,
                    base_url,
                    model,
                    vision_capable,
                    enabled,
                    timeout_ms,
                    concurrency,
                    extra_json: Some(extra.to_json_string()),
                })
            })
            .map_err(|e| format!("プロファイルの作成に失敗しました: {e}"))?,
    };

    // APIキー取り扱い (None=変更なし / Some("")=削除 / Some(値)=保存)
    match api_key.as_deref() {
        None => {}
        Some("") => {
            // DB参照を先にクリアし、成功後のみsecretを削除する
            // (逆順だとDB失敗時 credential が失われる — FG-WP10 非ブロッカー8)
            if let Some(old_ref) = record.api_key_ref.as_deref() {
                db.with_writer(|repos| repos.set_ai_profile_api_key_ref(record.id, None))
                    .map_err(|e| e.to_string())?;
                service.secret_store().delete(old_ref);
            }
        }
        Some(value) => {
            if value.trim().is_empty() {
                return Err("APIキーが空です".to_string());
            }
            let stored = service
                .secret_store()
                .save(&record.uuid, &ApiKey::new(value.trim().to_string()))
                .map_err(|e| format!("APIキーの保存に失敗しました: {e}"))?;
            if stored.kind == gento_ai::SecretStoreKind::PlainFile {
                tracing::warn!(
                    profile = record.id,
                    "APIキーを平文ファイルへ保存しました (資格情報ストア利用不可)"
                );
            }
            db.with_writer(|repos| {
                repos.set_ai_profile_api_key_ref(record.id, Some(&stored.as_ref_string()))
            })
            .map_err(|e| e.to_string())?;
        }
    }
    let reloaded = db
        .with_reader(|repos| repos.get_ai_profile(record.id))
        .map_err(|e| e.to_string())?
        .ok_or("保存済みプロファイルが読み取れません")?;
    let key = reloaded
        .api_key_ref
        .as_deref()
        .and_then(|r| service.secret_store().load(r));
    Ok(profile_dto(&reloaded, key.as_ref()))
}

#[tauri::command]
pub async fn ai_profile_delete(
    state: State<'_, AppState>,
    profile_id: String,
) -> Result<(), String> {
    let service = ai_service_of(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        let id = parse_i64(&profile_id, "プロファイルID")?;
        let db = service.db();
        let deleted = db
            .with_writer(|repos| repos.delete_ai_profile(id))
            .map_err(|e| format!("削除に失敗しました: {e}"))?;
        if let Some(key_ref) = deleted.api_key_ref.as_deref() {
            service.secret_store().delete(key_ref);
        }
        // pendingジョブの後始末 (失敗確定ではなくキャンセル)
        let cancelled = db
            .with_writer(|repos| repos.cancel_jobs_for_ai_profile(id))
            .unwrap_or(0);
        tracing::info!(
            profile = id,
            cancelled_jobs = cancelled,
            "ai profile deleted"
        );
        Ok(())
    })
    .await
    .map_err(|e| format!("削除に失敗しました: {e}"))?
}

// ===== Commands (テスト機能 4種) =====

macro_rules! run_test {
    ($state:expr, $profile_id:expr, $body:expr) => {{
        let service = ai_service_of(&$state)?;
        let profile_id = $profile_id;
        tauri::async_runtime::spawn_blocking(move || {
            let id = parse_i64(&profile_id, "プロファイルID")?;
            let service = service;
            let profile = service
                .db()
                .with_reader(|repos| repos.get_ai_profile(id))
                .map_err(|e| e.to_string())?
                .ok_or("プロファイルが見つかりません")?;
            // 鍵参照があるのに取得失敗なら送信しない (フェイルクローズ)
            let api_key = match profile.api_key_ref.as_deref() {
                None => None,
                Some(key_ref) => service.secret_store().load(key_ref),
            };
            if profile.api_key_ref.is_some() && api_key.is_none() {
                return Err(
                    "資格情報の取得に失敗しました (プロファイルのAPIキーが読み取れません)"
                        .to_string(),
                );
            }
            let extra = ProfileExtra::parse(profile.extra_json.as_deref());
            let client = AiClient::new(
                AiEndpoint {
                    base_url: profile.base_url.clone(),
                    model: profile.model.clone(),
                    timeout: Duration::from_millis(profile.timeout_ms as u64),
                },
                api_key,
                extra.send_response_format,
            );
            Ok($body(&client))
        })
        .await
        .map_err(|e| format!("テスト実行に失敗しました: {e}"))?
    }};
}

#[tauri::command]
pub async fn ai_profile_test_connection(
    state: State<'_, AppState>,
    profile_id: String,
) -> Result<gento_ai::TestReport, String> {
    run_test!(state, profile_id, test_connection)
}

#[tauri::command]
pub async fn ai_profile_test_text(
    state: State<'_, AppState>,
    profile_id: String,
) -> Result<gento_ai::TestReport, String> {
    run_test!(state, profile_id, test_text)
}

#[tauri::command]
pub async fn ai_profile_test_vision(
    state: State<'_, AppState>,
    profile_id: String,
) -> Result<gento_ai::TestReport, String> {
    run_test!(state, profile_id, test_vision)
}

#[tauri::command]
pub async fn ai_profile_test_structured(
    state: State<'_, AppState>,
    profile_id: String,
) -> Result<gento_ai::TestReport, String> {
    run_test!(state, profile_id, test_structured)
}

// ===== Commands (ジョブ) =====

/// 選択assetへジョブを投入 (手動再生成)。`force` = 成功済みでも再実行。
#[tauri::command]
pub async fn ai_jobs_enqueue(
    state: State<'_, AppState>,
    asset_ids: Vec<String>,
    force: Option<bool>,
) -> Result<u64, String> {
    let service = ai_service_of(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        enqueue_assets_impl(&service, &asset_ids, force.unwrap_or(false))
    })
    .await
    .map_err(|e| format!("ジョブ投入に失敗しました: {e}"))?
}

pub(crate) fn enqueue_assets_impl(
    service: &AiService,
    asset_ids: &[String],
    force: bool,
) -> Result<u64, String> {
    let mut ids = Vec::with_capacity(asset_ids.len());
    for id in asset_ids {
        ids.push(parse_i64(id, "AssetId")?);
    }
    let db = service.db();
    // 投入前にassetの存在検証 (不在IDは明確なエラーにする)
    for asset_id in &ids {
        let found = db
            .with_reader(|repos| {
                use gento_core::repo::AssetRepo as _;
                repos.get_asset(*asset_id).map(|a| a.is_some())
            })
            .unwrap_or(false);
        if !found {
            return Err(format!("画像が見つかりません (AssetId {asset_id})"));
        }
    }
    let profile = db
        .with_reader(|repos| repos.first_enabled_ai_profile())
        .map_err(|e| e.to_string())?
        .ok_or("有効なAIプロファイルがありません。先にプロファイルを作成してください")?;
    let mut enqueued = 0u64;
    for asset_id in ids {
        // 手動force無効時は成功済みassetをスキップ (§11.4: 既存結果の保持)
        if !force {
            let done = db
                .with_reader(|repos| {
                    repos.has_succeeded_ai_run(
                        asset_id,
                        "caption_and_tags",
                        gento_ai::PROMPT_VERSION,
                    )
                })
                .unwrap_or(false);
            if done {
                continue;
            }
        }
        let payload = json!({"profile_id": profile.id, "asset_id": asset_id}).to_string();
        let inserted = db
            .with_writer(|repos| {
                repos.enqueue_job(
                    JOB_TYPE_AI_CAPTION_TAGS,
                    "asset",
                    asset_id,
                    JOB_PRIORITY,
                    &payload,
                )
            })
            .map_err(|e| format!("ジョブ投入に失敗しました: {e}"))?;
        if inserted.is_some() {
            enqueued += 1;
        }
    }
    if enqueued > 0 {
        service.wake_all();
    }
    Ok(enqueued)
}

/// ルート内の未処理asset数 (初回全件処理の確認ダイアログ用)。
#[tauri::command]
pub async fn ai_root_pending_count(
    state: State<'_, AppState>,
    root_id: String,
) -> Result<u64, String> {
    let service = ai_service_of(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        let root = parse_i64(&root_id, "ルートID")?;
        let db = service.db();
        let count = db
            .with_reader(|repos| -> Result<i64, gento_core::repo::RepoError> {
                repos
                    .conn()
                    .query_row(
                        "SELECT count(*) FROM files f JOIN assets a ON a.id = f.asset_id \
                         WHERE f.root_id = ?1 AND f.scan_state = 'present' \
                         AND NOT EXISTS (SELECT 1 FROM ai_runs r WHERE r.asset_id = a.id \
                             AND r.operation = 'caption_and_tags' \
                             AND r.prompt_version = ?2 AND r.status = 'succeeded')",
                        [root, gento_ai::PROMPT_VERSION],
                        |row| row.get(0),
                    )
                    .map_err(|e| gento_core::repo::RepoError::Db(e.to_string()))
            })
            .map_err(|e| format!("件数の取得に失敗しました: {e}"))?;
        Ok(count.max(0) as u64)
    })
    .await
    .map_err(|e| format!("件数の取得に失敗しました: {e}"))?
}

/// ルート内の全未処理assetへ一括投入。UI確認後 (`confirmed=true`) のみ実行
/// (spec §11.3「件数を提示してから開始する」のbackend側ゲート)。
#[tauri::command]
pub async fn ai_jobs_enqueue_all_for_root(
    state: State<'_, AppState>,
    root_id: String,
    confirmed: bool,
) -> Result<u64, String> {
    let service = ai_service_of(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        enqueue_all_for_root_impl(&service, &root_id, confirmed)
    })
    .await
    .map_err(|e| format!("一括投入に失敗しました: {e}"))?
}

/// ルート内の全未処理assetへ一括投入 (確認ゲートはcommand側注釈参照)。
pub(crate) fn enqueue_all_for_root_impl(
    service: &AiService,
    root_id: &str,
    confirmed: bool,
) -> Result<u64, String> {
    {
        if !confirmed {
            return Err(
                "確認が済んでいません (件数を提示してユーザー承認後に実行してください)".to_string(),
            );
        }
        let root = parse_i64(root_id, "ルートID")?;
        let db = service.db();
        let profile = db
            .with_reader(|repos| repos.first_enabled_ai_profile())
            .map_err(|e| e.to_string())?
            .ok_or("有効なAIプロファイルがありません")?;
        let asset_ids: Vec<i64> = db
            .with_reader(|repos| -> Result<Vec<i64>, gento_core::repo::RepoError> {
                let mut stmt = repos
                    .conn()
                    .prepare(
                        "SELECT a.id FROM files f JOIN assets a ON a.id = f.asset_id \
                     WHERE f.root_id = ?1 AND f.scan_state = 'present' \
                     AND NOT EXISTS (SELECT 1 FROM ai_runs r WHERE r.asset_id = a.id \
                         AND r.operation = 'caption_and_tags' \
                         AND r.prompt_version = ?2 AND r.status = 'succeeded') \
                     GROUP BY a.id ORDER BY a.id",
                    )
                    .map_err(|e| gento_core::repo::RepoError::Db(e.to_string()))?;
                let rows = stmt
                    .query_map([root, gento_ai::PROMPT_VERSION], |row| row.get::<_, i64>(0))
                    .map_err(|e| gento_core::repo::RepoError::Db(e.to_string()))?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| gento_core::repo::RepoError::Db(e.to_string()))?;
                Ok(rows)
            })
            .map_err(|e| format!("対象の取得に失敗しました: {e}"))?;
        let mut enqueued = 0u64;
        for asset_id in asset_ids {
            let payload = json!({"profile_id": profile.id, "asset_id": asset_id}).to_string();
            let inserted = db
                .with_writer(|repos| {
                    repos.enqueue_job(
                        JOB_TYPE_AI_CAPTION_TAGS,
                        "asset",
                        asset_id,
                        JOB_PRIORITY,
                        &payload,
                    )
                })
                .map_err(|e| format!("ジョブ投入に失敗しました: {e}"))?;
            if inserted.is_some() {
                enqueued += 1;
            }
        }
        if enqueued > 0 {
            service.wake_all();
        }
        Ok(enqueued)
    }
}

#[tauri::command]
pub async fn ai_jobs_list(
    state: State<'_, AppState>,
    status: Option<String>,
    limit: Option<u32>,
) -> Result<serde_json::Value, String> {
    let service = ai_service_of(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        let limit = limit.unwrap_or(100).clamp(1, 500);
        let db = service.db();
        let jobs = db
            .with_reader(|repos| repos.list_jobs(limit))
            .map_err(|e| format!("ジョブ一覧の取得に失敗しました: {e}"))?;
        let counts: HashMap<String, u64> = db
            .with_reader(|repos| repos.job_status_counts())
            .unwrap_or_default()
            .into_iter()
            .collect();
        let jobs: Vec<AiJobDto> = match status.as_deref() {
            None | Some("") => jobs.iter().map(job_dto).collect(),
            Some(filter) => jobs
                .iter()
                .filter(|j| j.status == filter)
                .map(job_dto)
                .collect(),
        };
        Ok(json!({
            "jobs": jobs,
            "counts": counts,
            "paused": service.is_paused(),
        }))
    })
    .await
    .map_err(|e| format!("ジョブ一覧の取得に失敗しました: {e}"))?
}

#[tauri::command]
pub async fn ai_job_cancel(state: State<'_, AppState>, job_id: String) -> Result<(), String> {
    let service = ai_service_of(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        let id = parse_i64(&job_id, "ジョブID")?;
        service
            .db()
            .with_writer(|repos| repos.cancel_job(id))
            .map_err(|e| match e {
                gento_core::repo::RepoError::Constraint(msg) => {
                    format!("キャンセルできません: {msg}")
                }
                other => format!("キャンセルに失敗しました: {other}"),
            })?;
        service.wake_all();
        Ok(())
    })
    .await
    .map_err(|e| format!("キャンセルに失敗しました: {e}"))?
}

#[tauri::command]
pub async fn ai_jobs_pause(state: State<'_, AppState>) -> Result<(), String> {
    let service = ai_service_of(&state)?;
    service.set_paused(true);
    Ok(())
}

#[tauri::command]
pub async fn ai_jobs_resume(state: State<'_, AppState>) -> Result<(), String> {
    let service = ai_service_of(&state)?;
    service.set_paused(false);
    Ok(())
}

#[tauri::command]
pub async fn ai_jobs_retry_failed(
    state: State<'_, AppState>,
    job_ids: Option<Vec<String>>,
) -> Result<u64, String> {
    let service = ai_service_of(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        let ids: Vec<i64> = job_ids
            .map(|raw| {
                raw.iter()
                    .map(|id| parse_i64(id, "ジョブID"))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
            .unwrap_or_default();
        let restored = service
            .db()
            .with_writer(|repos| repos.retry_failed_jobs(&ids))
            .map_err(|e| format!("再試行の設定に失敗しました: {e}"))?;
        service.wake_all();
        Ok(restored)
    })
    .await
    .map_err(|e| format!("再試行の設定に失敗しました: {e}"))?
}

#[tauri::command]
pub async fn ai_run_history(
    state: State<'_, AppState>,
    asset_id: String,
) -> Result<Vec<AiRunDto>, String> {
    let service = ai_service_of(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        let id = parse_i64(&asset_id, "AssetId")?;
        let runs = service
            .db()
            .with_reader(|repos| repos.list_ai_runs_for_asset(id, 50))
            .map_err(|e| format!("履歴の取得に失敗しました: {e}"))?;
        Ok(runs
            .iter()
            .map(|run| AiRunDto {
                id: run.id.to_string(),
                profile_id: run.profile_id.map(|p| p.to_string()),
                operation: run.operation.clone(),
                status: run.status.clone(),
                prompt_version: run.prompt_version,
                request_summary: run
                    .request_summary
                    .as_deref()
                    .and_then(|s| serde_json::from_str(s).ok()),
                error: run.error.clone(),
                started_at: run.started_at.clone(),
                finished_at: run.finished_at.clone(),
            })
            .collect())
    })
    .await
    .map_err(|e| format!("履歴の取得に失敗しました: {e}"))?
}

/// ルート毎の自動処理設定 (ai_enabled / send_exif / send_gps)。
#[tauri::command]
pub async fn ai_root_settings_set(
    state: State<'_, AppState>,
    root_id: String,
    ai_enabled: bool,
    send_exif: bool,
    send_gps: bool,
) -> Result<(), String> {
    let service = ai_service_of(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        use gento_core::repo::{LibraryRootRepo, RootSettings};
        let id = parse_i64(&root_id, "ルートID")?;
        service
            .db()
            .with_writer(|repos| {
                let current = repos
                    .get_root(id)?
                    .ok_or(gento_core::repo::RepoError::NotFound)?;
                repos.set_root_settings(
                    id,
                    RootSettings {
                        watch_enabled: current.watch_enabled,
                        ai_enabled,
                        send_exif,
                        send_gps,
                    },
                )
            })
            .map_err(|e| format!("自動処理設定の保存に失敗しました: {e}"))?;
        Ok(())
    })
    .await
    .map_err(|e| format!("自動処理設定の保存に失敗しました: {e}"))?
}

/// スキャンでassetが確定した際の自動処理フック (library.rsのHashJobから)。
/// ルートai_enabled=1かつ有効プロファイルで未処理の場合のみ投入する。
/// ワーカーは5秒以内のポーリングで拾う (HashJob側にservice参照が無いため
/// 即時起床は行わない — 自動処理に即時性要件は無い)。
pub(crate) fn on_asset_linked(db: &Arc<Db>, file_id: i64, asset_id: i64) {
    let root_id: Option<i64> = db
        .with_reader(|repos| {
            use gento_core::repo::FileRepo as _;
            Ok(repos.get_file(file_id)?.map(|file| file.root_id))
        })
        .unwrap_or(None);
    let Some(root_id) = root_id else {
        return;
    };
    let should = db
        .with_reader(|repos| {
            let Some(root) = repos.get_root(root_id)? else {
                return Ok(false);
            };
            if !root.enabled || !root.ai_enabled {
                return Ok(false);
            }
            if repos.first_enabled_ai_profile()?.is_none() {
                return Ok(false);
            }
            Ok(!repos.has_succeeded_ai_run(
                asset_id,
                "caption_and_tags",
                gento_ai::PROMPT_VERSION,
            )?)
        })
        .unwrap_or(false);
    if !should {
        return;
    }
    let payload = json!({"asset_id": asset_id}).to_string();
    let _ = db.with_writer(|repos| {
        repos.enqueue_job(
            JOB_TYPE_AI_CAPTION_TAGS,
            "asset",
            asset_id,
            JOB_PRIORITY,
            &payload,
        )
    });
}

/// Tauriイベント送出実装 (emitterスレッドから)。
pub(crate) fn tauri_event_sink(app: AppHandle) -> Arc<EventSink> {
    Arc::new(move |payload: &AiJobsEventPayload| {
        let _ = app.emit("ai:jobs", payload);
    })
}

// ===== テスト (サービスレベルE2E: mock OpenAI互換endpoint相手) =====

#[cfg(test)]
mod tests {
    use super::*;
    use gento_ai::testserver::{MockBehavior, MockServer};
    use gento_core::assetmeta::{AssetMetadataParsed, AssetMetadataRepo, MetadataColumns};
    use gento_core::metadata::{AssetTagRepo, CaptionKind, CaptionRepo};
    use gento_core::repo::{
        AssetRepo, FileRepo, FileUpsert, LibraryRootRepo, NewAsset, RootSettings, ScanState,
    };
    use gento_db::NewAiProfile;
    use std::sync::Mutex as StdMutex;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "gento-tauri-ai-{tag}-{}-{}",
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

    fn test_png(path: &std::path::Path) {
        use image::{Rgb, RgbImage};
        let img = RgbImage::from_fn(64, 48, |x, _| Rgb([(x % 256) as u8, 0x80, 0x40]));
        img.write_with_encoder(image::codecs::png::PngEncoder::new(
            std::fs::File::create(path).unwrap(),
        ))
        .unwrap();
    }

    /// DB + ルート + asset + 代表ファイル。
    fn fixture(tag: &str) -> (TempDir, Arc<Db>, i64, i64) {
        let tmp = TempDir::new(tag);
        let root_dir = tmp.0.join("photos");
        std::fs::create_dir_all(&root_dir).unwrap();
        test_png(&root_dir.join("a.png"));
        let db = Arc::new(Db::open(tmp.0.join("db.sqlite3")).unwrap());
        let (root_id, asset_id) = db
            .with_writer(|repos| {
                let root = repos.add_root(&root_dir)?;
                let file = repos.upsert_file(FileUpsert {
                    root_id: root.id,
                    rel_path: PathBuf::from("a.png"),
                    display_name: "a.png".to_string(),
                    byte_size: 10,
                    mtime_ms: 1,
                    mtime_ns: 1_000,
                    scan_state: ScanState::Present,
                    os_file_id: None,
                    detach_asset: false,
                })?;
                let asset = repos.create_asset(NewAsset {
                    content_hash: format!("blake3:{tag}"),
                    hash_algorithm: "blake3".to_string(),
                    byte_size: 10,
                    mime_type: Some("image/png".to_string()),
                    width: Some(64),
                    height: Some(48),
                    has_alpha: Some(false),
                    is_animated: Some(false),
                    frame_count: None,
                })?;
                repos.link_file(file.id, asset.id)?;
                Ok((root.id, asset.id))
            })
            .unwrap();
        (tmp, db, root_id, asset_id)
    }

    fn add_profile(db: &Arc<Db>, base_url: &str) -> i64 {
        db.with_writer(|repos| {
            repos.insert_ai_profile(&NewAiProfile {
                display_name: "mock".to_string(),
                base_url: base_url.to_string(),
                model: "mock-vision-model".to_string(),
                vision_capable: true,
                enabled: true,
                timeout_ms: 8_000,
                concurrency: 2,
                extra_json: None,
            })
        })
        .unwrap()
        .id
    }

    /// wait_until: 条件が真になるかタイムアウトまでポーリング。
    fn wait_until(timeout_ms: u64, f: impl Fn() -> bool) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
        while std::time::Instant::now() < deadline {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        f()
    }

    fn job_status(db: &Arc<Db>) -> Vec<(String, String)> {
        db.with_reader(|repos| {
            Ok(repos
                .list_jobs(20)?
                .into_iter()
                .map(|j| (j.id.to_string(), j.status))
                .collect())
        })
        .unwrap()
    }

    /// イベント記録sink。
    fn recorder() -> (Arc<StdMutex<Vec<AiJobsEventPayload>>>, Arc<EventSink>) {
        let log: Arc<StdMutex<Vec<AiJobsEventPayload>>> = Arc::new(StdMutex::new(Vec::new()));
        let sink: Arc<EventSink> = {
            let log = Arc::clone(&log);
            Arc::new(move |payload: &AiJobsEventPayload| {
                log.lock().unwrap().push(payload.clone());
            })
        };
        (log, sink)
    }

    #[test]
    fn service_e2e_job_succeeds_and_emits_batched_events() {
        let server = MockServer::start(MockBehavior::default());
        let (tmp, db, _root, asset_id) = fixture("svc-ok");
        let _profile_id = add_profile(&db, &server.base_url);
        let (log, sink) = recorder();
        let service = AiService::start(db, tmp.0.clone(), sink);

        let enqueued = enqueue_assets_impl(&service, &[asset_id.to_string()], false).unwrap();
        assert_eq!(enqueued, 1);

        // ワーカーが処理してsucceededになる (500msポーリング)
        assert!(
            wait_until(15_000, || job_status(service.db())
                .iter()
                .any(|(_, s)| s == "succeeded")),
            "job should succeed: {:?}",
            job_status(service.db())
        );
        // タグ/キャプション適用
        let applied = service
            .db()
            .with_reader(|repos| {
                let tags = repos.list_asset_tags(asset_id)?;
                let caption = repos.get_current_caption(asset_id, CaptionKind::Description)?;
                Ok((tags, caption))
            })
            .unwrap();
        assert!(applied.0.iter().any(|t| t.source.as_str() == "ai"));
        assert!(applied.1.is_some());
        // イベントがバッチemitされている (running/succeededを含む)
        assert!(
            wait_until(5_000, || log
                .lock()
                .unwrap()
                .iter()
                .any(|p| p.changes.iter().any(|c| c.status == "succeeded"))),
            "events: {:?}",
            log.lock().unwrap().len()
        );
        let payloads = log.lock().unwrap();
        // 変更はバッチ (500ms窓) にまとめられていること: 1ジョブの
        // running/succeededが単一ペイロードに届くか、遅くとも少数バッチ
        assert!(payloads.len() <= 3, "バッチ過多: {}", payloads.len());
        assert!(payloads.iter().any(|p| p.counts.contains_key("succeeded")));
        service.shutdown();
    }

    /// acceptance 2/3: endpoint停止中もファイラー操作が成立し、ジョブは
    /// backoffで待機、endpoint復帰 (mock再起動) 後に再試行で成功する。
    #[test]
    fn service_e2e_endpoint_down_backoff_and_filer_unblocked() {
        // 先に「死んだport」でプロファイルを作る
        let dead_port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            drop(listener);
            port
        };
        let (tmp, db, root, asset_id) = fixture("svc-down");
        let profile_id = add_profile(&db, &format!("http://127.0.0.1:{dead_port}/v1"));
        let (log, sink) = recorder();
        let service = AiService::start(Arc::clone(&db), tmp.0.clone(), sink);

        let payload = json!({"profile_id": profile_id, "asset_id": asset_id}).to_string();
        db.with_writer(|repos| {
            repos.enqueue_job(JOB_TYPE_AI_CAPTION_TAGS, "asset", asset_id, 5, &payload)
        })
        .unwrap()
        .expect("enqueue");

        // 接続拒否 → retry_waiting (backoff)
        assert!(
            wait_until(20_000, || job_status(&db)
                .iter()
                .any(|(_, s)| s == "retry_waiting")),
            "endpoint停止中はretry_waiting: {:?}",
            job_status(&db)
        );
        let job = db
            .with_reader(|repos| Ok(repos.list_jobs(10)?.into_iter().next().unwrap()))
            .unwrap();
        assert!(job.attempts >= 1);
        assert!(
            job.scheduled_at.is_some(),
            "backoff期限: {:?}",
            job.scheduled_at
        );

        // ファイラー機能 (読み取り+書き込み) はblockingされない
        let readable = db
            .with_reader(|repos| {
                Ok(repos
                    .conn()
                    .query_row(
                        "SELECT count(*) FROM files WHERE root_id = ?1",
                        [root],
                        |r| r.get::<_, i64>(0),
                    )
                    .unwrap())
            })
            .unwrap();
        assert_eq!(readable, 1);
        db.with_writer(|repos| repos.set_rating_batch(&[asset_id], 3))
            .unwrap();

        // endpoint復帰: 同一portでmockを起こし、手動再試行で成功させる
        // (backoff期限 (既定30s) を待たず — retry_failedの経路証明)
        let revived = MockServer::start_on_port(dead_port, MockBehavior::default());
        assert!(revived.base_url.ends_with(&format!(":{dead_port}/v1")));
        db.with_writer(|repos| repos.retry_failed_jobs(&[]))
            .unwrap();
        // retry_waiting → 失敗確定にするには期限が来ないと失敗にならない:
        // この時点ではretry_waitingのまま。直接queuedへ戻して再試行させる。
        db.with_writer(|repos| {
            repos.conn().execute(
                "UPDATE jobs SET status = 'queued', attempts = 0, scheduled_at = NULL WHERE status = 'retry_waiting'",
                [],
            ).map_err(|e| gento_core::repo::RepoError::Db(e.to_string()))
        })
        .unwrap();
        service.wake_all();
        assert!(
            wait_until(20_000, || job_status(&db)
                .iter()
                .any(|(_, s)| s == "succeeded")),
            "endpoint復帰後に成功: {:?}",
            job_status(&db)
        );
        // イベント (500msバッチ) の到着も待つ
        assert!(
            wait_until(5_000, || !log.lock().unwrap().is_empty()),
            "ai:jobsイベントが送出されている"
        );
        service.shutdown();
    }

    /// acceptance 3: 再起動後にジョブを再開できる (running→queued回復)。
    #[test]
    fn service_startup_recovers_interrupted_running_jobs() {
        let server = MockServer::start(MockBehavior::default());
        let (tmp, db, _root, asset_id) = fixture("svc-recover");
        let profile_id = add_profile(&db, &server.base_url);
        // service無しでジョブをrunningまで進め、放置 (クラッシュ模倣)
        let payload = json!({"profile_id": profile_id, "asset_id": asset_id}).to_string();
        let job = db
            .with_writer(|repos| {
                repos.enqueue_job(JOB_TYPE_AI_CAPTION_TAGS, "asset", asset_id, 5, &payload)
            })
            .unwrap()
            .unwrap();
        let claimed = db
            .with_writer(|repos| repos.claim_job_by_id(job.id, &rfc3339_after_now(0)))
            .unwrap()
            .expect("claim");
        assert_eq!(claimed.status, "running");

        // 起動 → 回復 → 実行
        let (_log, sink) = recorder();
        let service = AiService::start(Arc::clone(&db), tmp.0.clone(), sink);
        assert!(
            wait_until(20_000, || job_status(&db)
                .iter()
                .any(|(id, s)| id == &job.id.to_string() && s == "succeeded")),
            "再起動後に完了: {:?}",
            job_status(&db)
        );
        service.shutdown();
    }

    /// pause中はジョブが実行されず、resumeで再開する。
    #[test]
    fn service_pause_blocks_and_resume_continues() {
        let server = MockServer::start(MockBehavior::default());
        let (tmp, db, _root, asset_id) = fixture("svc-pause");
        let _profile_id = add_profile(&db, &server.base_url);
        let (_log, sink) = recorder();
        let service = AiService::start(Arc::clone(&db), tmp.0.clone(), sink);
        service.set_paused(true);
        assert!(service.is_paused());
        // ポーリング周期 (500ms) を超えて待ち、全workerがpauseを観測済みの
        // 状態で投入する (pause検査通過直後の反復がenqueueを跨ぐ競合を避ける)
        std::thread::sleep(Duration::from_millis(700));

        let payload = json!({"asset_id": asset_id}).to_string();
        db.with_writer(|repos| {
            repos.enqueue_job(JOB_TYPE_AI_CAPTION_TAGS, "asset", asset_id, 5, &payload)
        })
        .unwrap()
        .expect("enqueue");
        // 2.5秒待ってもqueuedのまま (pause中)
        std::thread::sleep(Duration::from_millis(2_500));
        assert!(
            job_status(&db).iter().all(|(_, s)| s == "queued"),
            "pause中は実行されない: {:?}",
            job_status(&db)
        );
        // resume → 実行される
        service.set_paused(false);
        assert!(
            wait_until(20_000, || job_status(&db)
                .iter()
                .any(|(_, s)| s == "succeeded")),
            "resume後に完了: {:?}",
            job_status(&db)
        );
        service.shutdown();
    }

    /// 自動処理フック: ルートai_enabled+プロファイル→投入、無効→非投入、
    /// 成功済み→再投入しない。
    #[test]
    fn auto_enqueue_hook_follows_root_settings_and_prior_success() {
        let server = MockServer::start(MockBehavior::default());
        let (tmp, db, root, asset_id) = fixture("svc-auto");
        let _profile_id = add_profile(&db, &server.base_url);

        // ルートai_enabled=OFF → 投入されない
        on_asset_linked(&db, file_id_of(&db, asset_id), asset_id);
        assert!(job_status(&db).is_empty());

        // ai_enabled=ON → 投入
        db.with_writer(|repos| {
            repos.set_root_settings(
                root,
                RootSettings {
                    watch_enabled: true,
                    ai_enabled: true,
                    send_exif: false,
                    send_gps: false,
                },
            )
        })
        .unwrap();
        on_asset_linked(&db, file_id_of(&db, asset_id), asset_id);
        assert_eq!(job_status(&db).len(), 1);

        // 成功させる (run_one_jobを直接ドライブ)
        let (_log, sink) = recorder();
        drop(sink);
        let in_flight = std::sync::Mutex::new(HashMap::new());
        let not_paused = AtomicBool::new(false);
        let (tx, _rx) = std::sync::mpsc::sync_channel(16);
        assert!(run_one_job(&db, &tmp.0, &not_paused, &in_flight, &tx));

        // 再トリガー → 成功済みのため再投入しない
        on_asset_linked(&db, file_id_of(&db, asset_id), asset_id);
        assert!(
            wait_until(2_000, || job_status(&db).len() == 1),
            "成功済みは再投入しない: {:?}",
            job_status(&db)
        );
    }

    fn file_id_of(db: &Arc<Db>, asset_id: i64) -> i64 {
        db.with_reader(|repos| {
            Ok(repos
                .conn()
                .query_row(
                    "SELECT id FROM files WHERE asset_id = ?1",
                    [asset_id],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap())
        })
        .unwrap()
    }

    /// fixtureに2つ目のasset+代表ファイルを追加する (同時実行試験用)。
    fn add_asset(db: &Arc<Db>, root_id: i64, tag: &str, file_name: &str) -> i64 {
        db.with_writer(|repos| {
            use gento_core::repo::{FileRepo, FileUpsert};
            let root = repos.get_root(root_id)?.unwrap();
            let path = root.path.join(file_name);
            test_png(&path);
            let file = repos.upsert_file(FileUpsert {
                root_id,
                rel_path: PathBuf::from(file_name),
                display_name: file_name.to_string(),
                byte_size: 10,
                mtime_ms: 1,
                mtime_ns: 1_000,
                scan_state: ScanState::Present,
                os_file_id: None,
                detach_asset: false,
            })?;
            let asset = repos.create_asset(NewAsset {
                content_hash: format!("blake3:{tag}"),
                hash_algorithm: "blake3".to_string(),
                byte_size: 10,
                mime_type: Some("image/png".to_string()),
                width: Some(64),
                height: Some(48),
                has_alpha: Some(false),
                is_animated: Some(false),
                frame_count: None,
            })?;
            repos.link_file(file.id, asset.id)?;
            Ok(asset.id)
        })
        .unwrap()
    }

    /// FG-WP10 blocker 1: IPC (save_profile_impl) は範囲外max_image_edgeを拒否。
    #[test]
    fn ipc_rejects_out_of_range_max_image_edge() {
        let (tmp, db, _root, _asset) = fixture("edge-ipc");
        let (_log, sink) = recorder();
        let service = AiService::start(db, tmp.0.clone(), sink);
        let base = json!({
            "displayName": "edge",
            "baseUrl": "http://127.0.0.1:9/v1",
            "model": "m",
        });
        for bad in [0u32, 63, 2049, 8192, u32::MAX] {
            let input = json!({
                "displayName": "edge",
                "baseUrl": "http://127.0.0.1:9/v1",
                "model": "m",
                "extra": { "max_image_edge": bad },
            });
            let err = save_profile_impl(&service, &input, None).unwrap_err();
            assert!(
                err.contains("64〜2048"),
                "拒否される ({bad}): {err} / input={input}"
            );
        }
        // 境界値・範囲内・既定は受理
        for ok in [64u32, 1024, 2048] {
            let input = json!({
                "displayName": "edge-ok",
                "baseUrl": "http://127.0.0.1:9/v1",
                "model": "m",
                "extra": { "max_image_edge": ok },
            });
            let dto = save_profile_impl(&service, &input, None).unwrap();
            assert_eq!(dto.extra["max_image_edge"].as_u64(), Some(ok as u64));
        }
        // extra省略 (UI互換) は既定1024
        let dto = save_profile_impl(&service, &base, None).unwrap();
        assert_eq!(dto.extra["max_image_edge"].as_u64(), Some(1024));
        service.shutdown();
    }

    /// FG-WP10 blocker 1: 既存プロファイル保存値 (0/8192) は読み込み時に
    /// クランプされてDTOへ出る (ProfileExtra::parse経路)。
    #[test]
    fn legacy_saved_out_of_range_edge_is_clamped_in_dto() {
        let (_tmp, db, _root, _asset) = fixture("edge-legacy");
        let profile_id = add_profile(&db, "http://127.0.0.1:9/v1");
        for (saved, clamped) in [(0u32, 64u32), (8192, 2048), (100, 100)] {
            db.with_writer(|repos| -> Result<(), gento_core::repo::RepoError> {
                // savedはu32のためSQL注入なし (値の直埋め)
                repos
                    .conn()
                    .execute(
                        &format!(
                            "UPDATE ai_profiles SET extra_json = '{{\"max_image_edge\":{saved}}}' \
                             WHERE id = {profile_id}"
                        ),
                        [],
                    )
                    .map_err(|e| gento_core::repo::RepoError::Db(e.to_string()))?;
                Ok(())
            })
            .unwrap();
            let record = db
                .with_reader(|repos| repos.get_ai_profile(profile_id))
                .unwrap()
                .unwrap();
            let dto = profile_dto(&record, None);
            assert_eq!(
                dto.extra["max_image_edge"].as_u64(),
                Some(clamped as u64),
                "保存値{saved} → クランプ{clamped}"
            );
        }
    }

    /// FG-WP10 blocker 2: 4xx (認証失敗) は1回で確定failed — 再送しない。
    #[test]
    fn service_permanent_4xx_fails_once_without_retry() {
        let server = MockServer::start(MockBehavior {
            chat_status: 401,
            ..MockBehavior::default()
        });
        let (tmp, db, _root, asset_id) = fixture("auth-fail");
        let profile_id = add_profile(&db, &server.base_url);
        let (_log, sink) = recorder();
        let service = AiService::start(Arc::clone(&db), tmp.0.clone(), sink);
        let payload = json!({"profile_id": profile_id, "asset_id": asset_id}).to_string();
        db.with_writer(|repos| {
            repos.enqueue_job(JOB_TYPE_AI_CAPTION_TAGS, "asset", asset_id, 5, &payload)
        })
        .unwrap()
        .expect("enqueue");

        assert!(
            wait_until(20_000, || job_status(&db)
                .iter()
                .any(|(_, s)| s == "failed")),
            "401は即時failed: {:?}",
            job_status(&db)
        );
        let job = db
            .with_reader(|repos| Ok(repos.list_jobs(10)?.into_iter().next().unwrap()))
            .unwrap();
        assert_eq!(job.attempts, 1, "1回の試行のみ");
        assert!(
            job.scheduled_at.is_none(),
            "retry_waitingに入らない (=再試行予定なし)"
        );
        assert!(job.error.as_deref().unwrap_or("").contains("401"));
        // 一定時間経過しても再送されない (画像再開示の反復を防止)
        std::thread::sleep(Duration::from_millis(2_000));
        assert_eq!(server.chat_calls(), 1, "リクエストは1回のみ");
        assert!(
            job_status(&db).iter().all(|(_, s)| s != "retry_waiting"),
            "retry_waitingへ遷移しない: {:?}",
            job_status(&db)
        );
        service.shutdown();
    }

    /// FG-WP10 blocker 2: 5xxはbackoff付きretry_waitingへ (接続拒否と対比)。
    #[test]
    fn service_5xx_goes_to_retry_waiting_with_backoff() {
        let server = MockServer::start(MockBehavior {
            chat_status: 500,
            ..MockBehavior::default()
        });
        let (tmp, db, _root, asset_id) = fixture("err5xx");
        let profile_id = add_profile(&db, &server.base_url);
        let (_log, sink) = recorder();
        let service = AiService::start(Arc::clone(&db), tmp.0.clone(), sink);
        let payload = json!({"profile_id": profile_id, "asset_id": asset_id}).to_string();
        db.with_writer(|repos| {
            repos.enqueue_job(JOB_TYPE_AI_CAPTION_TAGS, "asset", asset_id, 5, &payload)
        })
        .unwrap()
        .expect("enqueue");

        assert!(
            wait_until(20_000, || job_status(&db)
                .iter()
                .any(|(_, s)| s == "retry_waiting")),
            "500はretry_waiting: {:?}",
            job_status(&db)
        );
        let job = db
            .with_reader(|repos| Ok(repos.list_jobs(10)?.into_iter().next().unwrap()))
            .unwrap();
        assert!(job.attempts >= 1);
        assert!(
            job.scheduled_at.is_some(),
            "backoff期限が設定される: {:?}",
            job.scheduled_at
        );
        service.shutdown();
    }

    /// FG-WP10 blocker 3: concurrency=1のプロファイルで2worker同時ドライブ
    /// しても同時実行は起きない (遅延注入mockの同時処理数で決定的に検出)。
    #[test]
    fn concurrency_one_is_enforced_across_two_workers() {
        let server = MockServer::start(MockBehavior {
            delay: Duration::from_millis(400),
            ..MockBehavior::default()
        });
        let (tmp, db, root, asset_a) = fixture("conc1");
        let asset_b = add_asset(&db, root, "conc1-b", "b.png");
        // concurrency=1のプロファイル
        let profile_id = db
            .with_writer(|repos| {
                repos.insert_ai_profile(&NewAiProfile {
                    display_name: "serial".to_string(),
                    base_url: server.base_url.clone(),
                    model: "mock-vision-model".to_string(),
                    vision_capable: true,
                    enabled: true,
                    timeout_ms: 8_000,
                    concurrency: 1,
                    extra_json: None,
                })
            })
            .unwrap()
            .id;
        for asset in [asset_a, asset_b] {
            let payload = json!({"profile_id": profile_id, "asset_id": asset}).to_string();
            db.with_writer(|repos| {
                repos.enqueue_job(JOB_TYPE_AI_CAPTION_TAGS, "asset", asset, 5, &payload)
            })
            .unwrap()
            .expect("enqueue");
        }

        let in_flight = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let (tx, _rx) = std::sync::mpsc::sync_channel::<AiJobDto>(16);
        let all_terminal = |db: &Arc<Db>| {
            job_status(db)
                .iter()
                .all(|(_, s)| s == "succeeded" || s == "failed" || s == "cancelled")
        };
        let not_paused = Arc::new(AtomicBool::new(false));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let db = Arc::clone(&db);
            let data_dir = tmp.0.clone();
            let in_flight = Arc::clone(&in_flight);
            let not_paused = Arc::clone(&not_paused);
            let tx = tx.clone();
            workers.push(
                std::thread::Builder::new()
                    .spawn(move || {
                        while !all_terminal(&db) {
                            let ran = run_one_job(&db, &data_dir, &not_paused, &in_flight, &tx);
                            if !ran {
                                std::thread::sleep(Duration::from_millis(10));
                            }
                        }
                    })
                    .unwrap(),
            );
        }
        for handle in workers {
            handle.join().unwrap();
        }

        let statuses = job_status(&db);
        assert!(
            statuses.iter().all(|(_, s)| s == "succeeded"),
            "両ジョブ完了: {statuses:?}"
        );
        assert_eq!(
            server
                .log
                .concurrent_max
                .load(std::sync::atomic::Ordering::SeqCst),
            1,
            "mock同時処理数の最大は1 (=直列化されている)"
        );
        assert_eq!(server.chat_calls(), 2);
    }

    /// FG-WP10 blocker 5: 鍵参照があるが取得失敗 (ストア破損) → 送信なしの
    /// permanent失敗 (mockのBearer検証にリクエスト自体が届かない)。
    #[test]
    fn key_lookup_failure_fails_closed_without_sending() {
        let server = MockServer::start(MockBehavior {
            expected_bearer: Some("sk-must-not-be-sent".to_string()),
            ..MockBehavior::default()
        });
        let (tmp, db, _root, asset_id) = fixture("keyfail");
        let profile_id = add_profile(&db, &server.base_url);
        // 鍵をfileストアへ保存して参照を設定
        let secrets = SecretStore::file_only(&tmp.0);
        let record = db
            .with_reader(|repos| repos.get_ai_profile(profile_id))
            .unwrap()
            .unwrap();
        let stored = secrets
            .save(
                &record.uuid,
                &ApiKey::new("sk-must-not-be-sent".to_string()),
            )
            .unwrap();
        db.with_writer(|repos| {
            repos.set_ai_profile_api_key_ref(profile_id, Some(&stored.as_ref_string()))
        })
        .unwrap();
        // 「ストア破損」: 鍵ファイルを消失させる
        std::fs::remove_file(secrets.secret_dir().join(format!("{}.key", stored.id))).unwrap();

        let (_log, sink) = recorder();
        let service = AiService::start(Arc::clone(&db), tmp.0.clone(), sink);
        let payload = json!({"profile_id": profile_id, "asset_id": asset_id}).to_string();
        db.with_writer(|repos| {
            repos.enqueue_job(JOB_TYPE_AI_CAPTION_TAGS, "asset", asset_id, 5, &payload)
        })
        .unwrap()
        .expect("enqueue");

        assert!(
            wait_until(20_000, || job_status(&db)
                .iter()
                .any(|(_, s)| s == "failed")),
            "鍵取得失敗はpermanent失敗: {:?}",
            job_status(&db)
        );
        let job = db
            .with_reader(|repos| Ok(repos.list_jobs(10)?.into_iter().next().unwrap()))
            .unwrap();
        assert!(
            job.error
                .as_deref()
                .unwrap_or("")
                .contains("資格情報の取得に失敗"),
            "日本語エラー: {:?}",
            job.error
        );
        assert_eq!(job.attempts, 1, "再試行しない");
        // リクエスト自体が来ないこと (未認証送信の禁止の実証)
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(server.chat_calls(), 0, "chatへのリクエスト無し");
        assert_eq!(
            server
                .log
                .unauthorized
                .load(std::sync::atomic::Ordering::SeqCst),
            0,
            "認証失敗リクエストも無し (送信していない)"
        );
        service.shutdown();
    }

    /// FG-WP10 非ブロッカー8: 鍵クリアはDB参照クリア→成功後secret削除の順
    /// (成功時は両方の効果が観測される。保存先がkeyringでもfileでも成立)。
    #[test]
    fn key_clear_removes_db_ref_then_secret() {
        let (tmp, db, _root, _asset) = fixture("keyclear");
        let (_log, sink) = recorder();
        let service = AiService::start(Arc::clone(&db), tmp.0.clone(), sink);
        let input = json!({
            "displayName": "クリア",
            "baseUrl": "http://127.0.0.1:9/v1",
            "model": "m",
        });
        let dto = save_profile_impl(&service, &input, Some("sk-clear-me".to_string())).unwrap();
        assert!(dto.has_api_key);
        // 鍵が読み取れる (保存先がkeyring/fileのどちらでも同じAPIで検証)
        let store = SecretStore::new(&tmp.0);
        let record = db
            .with_reader(|repos| repos.get_ai_profile(dto.id.parse().unwrap()))
            .unwrap()
            .unwrap();
        let key_ref = record.api_key_ref.clone().unwrap();
        assert!(store.load(&key_ref).is_some(), "保存直後は読み取れる");

        // 空文字列で削除 (同一プロファイルのidを渡す) → 参照と実体の両方が消える
        let clear_input = json!({
            "id": dto.id,
            "displayName": "クリア",
            "baseUrl": "http://127.0.0.1:9/v1",
            "model": "m",
        });
        let cleared = save_profile_impl(&service, &clear_input, Some(String::new())).unwrap();
        assert!(!cleared.has_api_key);
        assert!(
            store.load(&key_ref).is_none(),
            "secret実体も削除されている (DB参照クリア後)"
        );
        service.shutdown();
    }

    /// save_profile_implのAPIキー保存: keyring不可環境ではfileフォールバック
    /// (警告ログ) でも参照が記録され、DTOには鍵本体が載らない。
    #[test]
    fn save_profile_stores_key_ref_not_secret_in_dto_or_logs() {
        let (tmp, db, _root, _asset) = fixture("svc-key");
        let (_log, sink) = recorder();
        let service = AiService::start(Arc::clone(&db), tmp.0.clone(), sink);
        let secret = "sk-tauri-test-do-not-log".to_string();

        let input = json!({
            "displayName": "キー付き",
            "baseUrl": "http://127.0.0.1:9/v1",
            "model": "m",
            "visionCapable": true,
            "enabled": true,
        });
        let dto = save_profile_impl(&service, &input, Some(secret.clone())).unwrap();
        assert!(dto.has_api_key);
        assert!(dto.api_key_store.is_some());
        let dto_json = serde_json::to_string(&dto).unwrap();
        assert!(!dto_json.contains(&secret), "DTOに鍵が含まれる: {dto_json}");

        // 通常ログ (tracing) に鍵が出ないこと: バッファsinkで検証
        let captured: Arc<StdMutex<Vec<u8>>> = Arc::new(StdMutex::new(Vec::new()));
        {
            use tracing_subscriber::fmt::MakeWriter;
            struct BufferWriter(Arc<StdMutex<Vec<u8>>>);
            impl std::io::Write for BufferWriter {
                fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                    self.0.lock().unwrap().extend_from_slice(buf);
                    Ok(buf.len())
                }
                fn flush(&mut self) -> std::io::Result<()> {
                    Ok(())
                }
            }
            struct Maker(Arc<StdMutex<Vec<u8>>>);
            impl<'a> MakeWriter<'a> for Maker {
                type Writer = BufferWriter;
                fn make_writer(&'a self) -> Self::Writer {
                    BufferWriter(Arc::clone(&self.0))
                }
            }
            let subscriber = tracing_subscriber::fmt()
                .with_writer(Maker(Arc::clone(&captured)))
                .with_max_level(tracing_subscriber::filter::LevelFilter::DEBUG)
                .finish();
            tracing::subscriber::with_default(subscriber, || {
                // 鍵を扱う操作一式
                let input2 = json!({
                    "displayName": "ログ検証",
                    "baseUrl": "http://127.0.0.1:9/v1",
                    "model": "m",
                });
                let dto2 = save_profile_impl(&service, &input2, Some(secret.clone())).unwrap();
                assert!(dto2.has_api_key);
                // 読み出し (パイプラインでの鍵解決を含む失敗リクエスト)
                let profile = service
                    .db()
                    .with_reader(|repos| repos.get_ai_profile(dto2.id.parse().unwrap()))
                    .unwrap()
                    .unwrap();
                let key = profile
                    .api_key_ref
                    .as_deref()
                    .and_then(|r| service.secret_store().load(r));
                assert!(key.is_some());
                let client = AiClient::new(
                    AiEndpoint {
                        base_url: profile.base_url.clone(),
                        model: profile.model.clone(),
                        timeout: Duration::from_millis(200),
                    },
                    key,
                    false,
                );
                let report = test_connection(&client);
                assert!(!report.ok, "到達不能endpoint: {report:?}");
                let _ = gento_ai::client::test_text(&client);
            });
        }
        let logs = String::from_utf8_lossy(&captured.lock().unwrap()).into_owned();
        assert!(!logs.contains(&secret), "ログに鍵が含まれる: {logs}");
        // DBファイル群にも鍵が無い
        let mut found = false;
        for entry in std::fs::read_dir(&tmp.0).unwrap() {
            let path = entry.unwrap().path();
            if path.is_file() && path.extension().and_then(|e| e.to_str()) != Some("png") {
                let bytes = std::fs::read(&path).unwrap();
                if bytes.windows(secret.len()).any(|w| w == secret.as_bytes()) {
                    found = true;
                }
            }
        }
        assert!(!found, "DB/WALに鍵平文が存在");
        service.shutdown();
    }

    /// enqueue_all_for_rootの確認ゲートと対象絞り込み。
    #[test]
    fn enqueue_all_requires_confirmation_and_skips_done() {
        let (tmp, db, root, asset_id) = fixture("svc-all");
        let _profile_id = add_profile(&db, "http://127.0.0.1:9/v1");
        let (_log, sink) = recorder();
        let service = AiService::start(Arc::clone(&db), tmp.0.clone(), sink);

        // confirmed=false は拒否
        let err = enqueue_all_for_root_impl(&service, &root.to_string(), false).unwrap_err();
        assert!(err.contains("確認"), "{err}");
        // confirmed=true で投入
        let n = enqueue_all_for_root_impl(&service, &root.to_string(), true).unwrap();
        assert_eq!(n, 1);
        // 成功扱いにしてから再実行 → 0件
        db.with_writer(|repos| {
            repos.insert_ai_run(
                asset_id,
                None,
                "caption_and_tags",
                gento_ai::PROMPT_VERSION,
                None,
            )
        })
        .unwrap();
        db.with_writer(|repos| {
            repos.conn().execute(
                "UPDATE ai_runs SET status = 'succeeded', finished_at = started_at WHERE asset_id = ?1",
                [asset_id],
            ).map_err(|e| gento_core::repo::RepoError::Db(e.to_string()))
        })
        .unwrap();
        let n2 = enqueue_all_for_root_impl(&service, &root.to_string(), true).unwrap();
        assert_eq!(n2, 0, "成功済みは対象外");
        service.shutdown();
    }

    /// ai_run_history (InfoPanelのAIバッジ照会)。
    #[test]
    fn run_history_records_request_summary_without_secret() {
        let server = MockServer::start(MockBehavior::default());
        let (tmp, db, _root, asset_id) = fixture("svc-hist");
        let profile_id = add_profile(&db, &server.base_url);
        let (_log, sink) = recorder();
        let service = AiService::start(Arc::clone(&db), tmp.0.clone(), sink);
        let payload = json!({"profile_id": profile_id, "asset_id": asset_id}).to_string();
        db.with_writer(|repos| {
            repos.enqueue_job(JOB_TYPE_AI_CAPTION_TAGS, "asset", asset_id, 5, &payload)
        })
        .unwrap();
        assert!(wait_until(15_000, || {
            !db.with_reader(|repos| repos.list_ai_runs_for_asset(asset_id, 10))
                .unwrap()
                .is_empty()
        }));
        let runs = db
            .with_reader(|repos| repos.list_ai_runs_for_asset(asset_id, 10))
            .unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].prompt_version, gento_ai::PROMPT_VERSION);
        let summary = runs[0].request_summary.as_deref().unwrap();
        assert!(summary.contains("prompt_version"));
        assert!(!summary.contains("sk-"), "概要に鍵を含まない: {summary}");
        service.shutdown();
    }

    /// GPSメタデータを持つassetでルートsend_gps=ONの場合、リクエストへ
    /// 座標が含まれる (tauriレイヤの自動パイプライン経由の確認)。
    #[test]
    fn auto_pipeline_sends_gps_only_when_enabled() {
        let server = MockServer::start(MockBehavior::default());
        let (tmp, db, root, asset_id) = fixture("svc-gps");
        let profile_id = add_profile(&db, &server.base_url);
        db.with_writer(|repos| {
            repos.save_asset_metadata(
                asset_id,
                &[AssetMetadataParsed {
                    parser_name: "exif".to_string(),
                    parser_version: "1".to_string(),
                    columns: MetadataColumns {
                        gps_latitude: Some(35.681236),
                        gps_longitude: Some(139.767125),
                        ..MetadataColumns::default()
                    },
                    raw_exif: None,
                    raw_iptc: None,
                    raw_png_metadata: None,
                    raw_workflow: None,
                }],
            )
        })
        .unwrap();
        // send_gps=ON
        db.with_writer(|repos| {
            repos.set_root_settings(
                root,
                RootSettings {
                    watch_enabled: true,
                    ai_enabled: false,
                    send_exif: true,
                    send_gps: true,
                },
            )
        })
        .unwrap();
        // プロファイル側もON (AND)
        db.with_writer(|repos| {
            repos.update_ai_profile_settings(&gento_db::AiProfileRecord {
                id: profile_id,
                uuid: "00000000-0000-7000-8000-0000000000aa".to_string(),
                display_name: "mock".into(),
                base_url: server.base_url.clone(),
                model: "mock-vision-model".into(),
                api_key_ref: None,
                vision_capable: true,
                enabled: true,
                timeout_ms: 8_000,
                concurrency: 2,
                extra_json: Some(r#"{"send_exif":true,"send_gps":true}"#.to_string()),
                created_at: String::new(),
                updated_at: String::new(),
            })
        })
        .unwrap();

        let (_log, sink) = recorder();
        let service = AiService::start(Arc::clone(&db), tmp.0.clone(), sink);
        let payload = json!({"profile_id": profile_id, "asset_id": asset_id}).to_string();
        db.with_writer(|repos| {
            repos.enqueue_job(JOB_TYPE_AI_CAPTION_TAGS, "asset", asset_id, 5, &payload)
        })
        .unwrap();
        assert!(wait_until(15_000, || {
            job_status(&db).iter().any(|(_, s)| s == "succeeded")
        }));
        let body = server.log.last_body.lock().unwrap().clone();
        assert!(body.contains("35.681236"), "GPS座標が送信される: {body}");
        service.shutdown();
    }
}
