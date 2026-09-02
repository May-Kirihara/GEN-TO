//! gento-embedding-worker — 独立埋め込みワーカープロセス (WP-11)。
//!
//! docs/embedding-runtime.md §5の実行アーキテクチャ:
//! - 推論エンジンの異常からメインUIを隔離する (クラッシュしても
//!   ホスト (gento-tauri) は継続し、再起動する)
//! - モデルの明示的ロード・アンロード (起動時自動ロードなし)
//! - アイドルアンロード (既定10分)
//! - 通信はstdio長さ前置JSON (`gento_embedding::worker_proto`)
//!
//! 使用: ホストがspawnする (引数なし)。単体実行時はstdinへフレームを
//! 流すことで手動ドライブも可能。

use std::io::{BufReader, BufWriter};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gento_embedding::manifest::{verify_package, PackageVerification};
use gento_embedding::provider::{EmbeddingProvider, RuriOnnxProvider};
use gento_embedding::worker_proto::{
    read_frame, write_frame, ModelInfo, WorkerRequest, WorkerResponse, WorkerResult, WorkerStatus,
};

/// アイドルアンロードまでの既定時間 (設計§8「一定時間未使用なら
/// アンロード可能にする」。WP-11仕様: 10分)。
const IDLE_UNLOAD: Duration = Duration::from_secs(10 * 60);

/// 状態管理はワーカースレッド内で完結 (外部同期なし)。
struct WorkerState {
    provider: Option<RuriOnnxProvider>,
    loaded_at: Option<Instant>,
    last_used: Option<Instant>,
    queries_processed: u64,
    documents_processed: u64,
    loaded_model: Option<String>,
}

impl WorkerState {
    fn new() -> Self {
        Self {
            provider: None,
            loaded_at: None,
            last_used: None,
            queries_processed: 0,
            documents_processed: 0,
            loaded_model: None,
        }
    }

    fn unload(&mut self) {
        if self.provider.is_some() {
            tracing::info!(model = ?self.loaded_model, "unloading model");
        }
        self.provider = None;
        self.loaded_at = None;
        self.last_used = None;
        self.loaded_model = None;
    }
}

fn main() {
    // ワーカーのログはstderrへ (stdoutはプロトコルチャネル)。
    // 既定は警告のみ。GENTO_EMBEDDING_WORKER_LOG=debug で詳細化。
    let filter = tracing_subscriber::EnvFilter::try_from_env("GENTO_EMBEDDING_WORKER_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
    let stdout = std::io::stdout();
    let mut writer = BufWriter::new(stdout.lock());

    // 読み取りスレッド: stdin → チャネル。メインループはidle unloadの
    // ためにタイムアウト付き受信が必要なため分離する (StdinLockは
    // Sendでないためlockはスレッド内で取得)。
    let (tx, rx): (mpsc::SyncSender<WorkerRequest>, Receiver<WorkerRequest>) =
        mpsc::sync_channel(16);
    std::thread::Builder::new()
        .name("worker-stdin".into())
        .spawn(move || {
            let stdin = std::io::stdin();
            let mut reader = BufReader::new(stdin.lock());
            loop {
                match read_frame::<WorkerRequest>(&mut reader) {
                    Ok(Some(request)) => {
                        if tx.send(request).is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        tracing::error!(%error, "protocol frame error");
                        break;
                    }
                }
            }
        })
        .expect("spawn stdin reader");

    let state = Arc::new(Mutex::new(WorkerState::new()));
    // 初期状態を1回だけ報告しない (ホストはstatusで能動取得)
    loop {
        let timeout = {
            let state = state.lock().unwrap_or_else(|p| p.into_inner());
            match (state.provider.is_some(), state.last_used) {
                (true, Some(last_used)) => IDLE_UNLOAD.saturating_sub(last_used.elapsed()),
                _ => Duration::from_secs(3600),
            }
        };
        let request = match rx.recv_timeout(timeout.min(Duration::from_secs(1))) {
            Ok(request) => request,
            Err(RecvTimeoutError::Timeout) => {
                // 1秒毎の TICK: アイドルアンロード判定
                let mut state = state.lock().unwrap_or_else(|p| p.into_inner());
                if state.provider.is_some() {
                    if let Some(last_used) = state.last_used {
                        if last_used.elapsed() >= IDLE_UNLOAD {
                            tracing::info!("idle unload");
                            state.unload();
                        }
                    }
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => {
                // stdin閉鎖 = ホスト終了。即座に終了する。
                break;
            }
        };
        let response = handle_request(&state, request);
        let is_shutdown = matches!(&response.result, Some(WorkerResult::Shutdown));
        if let Err(error) = write_frame(&mut writer, &response) {
            tracing::error!(%error, "response write failed");
            break;
        }
        if is_shutdown {
            tracing::info!("shutdown requested");
            break;
        }
    }
}

fn handle_request(state: &Arc<Mutex<WorkerState>>, request: WorkerRequest) -> WorkerResponse {
    let mut state = state.lock().unwrap_or_else(|p| p.into_inner());
    match request {
        WorkerRequest::Load {
            id,
            model,
            models_dir,
            threads,
        } => {
            match verify_package(std::path::Path::new(&models_dir), &model) {
                PackageVerification::Verified => {}
                other => {
                    return WorkerResponse::error(
                        id,
                        format!("モデルパッケージ検証失敗 ({model}): {}", other.describe()),
                    )
                }
            }
            let threads = threads.unwrap_or_else(RuriOnnxProvider::default_threads);
            tracing::info!(model = %model, threads, "loading model");
            match RuriOnnxProvider::load(
                &model,
                &std::path::Path::new(&models_dir).join(&model),
                Some(threads),
            ) {
                Ok(provider) => {
                    let info = to_proto_model_info(provider.model_info());
                    state.provider = Some(provider);
                    state.loaded_at = Some(Instant::now());
                    state.last_used = Some(Instant::now());
                    state.loaded_model = Some(model);
                    WorkerResponse::ok(id, WorkerResult::Load { model_info: info })
                }
                Err(error) => WorkerResponse::error(id, error.to_string()),
            }
        }
        WorkerRequest::Unload { id } => {
            state.unload();
            WorkerResponse::ok(id, WorkerResult::Unload)
        }
        WorkerRequest::EmbedQuery { id, text } => {
            let Some(provider) = state.provider.as_mut() else {
                return WorkerResponse::error(id, "モデルがロードされていません");
            };
            match provider.embed_query(&text) {
                Ok(vector) => {
                    state.last_used = Some(Instant::now());
                    state.queries_processed += 1;
                    WorkerResponse::ok(id, WorkerResult::EmbedQuery { vector })
                }
                Err(error) => WorkerResponse::error(id, error.to_string()),
            }
        }
        WorkerRequest::EmbedDocuments { id, texts } => {
            let Some(provider) = state.provider.as_mut() else {
                return WorkerResponse::error(id, "モデルがロードされていません");
            };
            match provider.embed_documents(&texts) {
                Ok(vectors) => {
                    state.last_used = Some(Instant::now());
                    state.documents_processed += texts.len() as u64;
                    WorkerResponse::ok(id, WorkerResult::EmbedDocuments { vectors })
                }
                Err(error) => WorkerResponse::error(id, error.to_string()),
            }
        }
        WorkerRequest::Status { id } => {
            let loaded_for_ms = state
                .loaded_at
                .map(|at| at.elapsed().as_millis() as u64)
                .unwrap_or(0);
            let idle_for_ms = state.last_used.map(|at| at.elapsed().as_millis() as u64);
            WorkerResponse::ok(
                id,
                WorkerResult::Status {
                    status: WorkerStatus {
                        loaded_model: state.loaded_model.clone(),
                        loaded_for_ms,
                        idle_for_ms,
                        queries_processed: state.queries_processed,
                        documents_processed: state.documents_processed,
                    },
                },
            )
        }
        WorkerRequest::Shutdown { id } => WorkerResponse::ok(id, WorkerResult::Shutdown),
    }
}

fn to_proto_model_info(info: gento_embedding::provider::ModelInfo) -> ModelInfo {
    ModelInfo {
        model_id: info.model_id,
        package_name: info.package_name,
        package_version: info.package_version,
        repository_id: info.repository_id,
        revision: info.revision,
        license: info.license,
        embedding_dim: info.embedding_dim,
        max_input_length: info.max_input_length,
        distance_metric: info.distance_metric.to_string(),
    }
}
