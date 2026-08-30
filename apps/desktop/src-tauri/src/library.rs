//! 登録ルート (ライブラリ) の実行時制御 (MVP WP-2)。
//!
//! - `LibraryController`: スキャンの逐次実行ワーカー・BLAKE3ハッシュプール
//!   (Background優先度・gento-image WorkerPoolを再利用)・ルートごとの
//!   フォルダ監視 (notify/inotify) を統括する。
//! - ドメインロジック (差分アルゴリズム・イベント結合) は gento-core
//!   `library` にあり、ここはTauri側の接着のみ。
//!
//! # 監視プロトコル (Codex計画レビュー指摘の反映)
//!
//! ```text
//! 監視確立 (watcher開始) → イベントバッファリング → 初回スキャン
//!   → キューイベント適用 (スコープ差分スキャン = reconciliation)
//! ```
//!
//! - 監視イベントはdebounceウィンドウ (400ms) 内でパス単位に結合され、
//!   影響を受けたディレクトリのスコープ差分スキャンとして適用される。
//!   差分スキャンはファイルシステムの最終状態のみに依存して冪等のため、
//!   バースト後も最終状態が一致する (inotifyイベント欠落を含む)。
//! - イベント欠落 (overflow等) は通常イベントが無くても検出・全体再スキャンで
//!   収束する (overflowフラグはdebounceループのtickで拾われる)。
//! - ルート削除・アンマウント: watcherを解放し `roots:changed` を通知。
//!   DBのルート行とFile行は保持 (「不在ルート=無効表示、削除しない」)。
//!   ルート再出現時はメンテナンス周期でwatcherを再確立する。
//! - watcher確立失敗 (inotify上限等) は `scan_status.watch_degraded` で
//!   露出し、メンテナンス周期でのポーリング再スキャンで劣化運用する。
//!
//! # スキャンの逐次化
//!
//! 全コマンド (全体スキャン・スコープ差分) は単一ワーカースレッドの
//! キューへ投入され順次実行される。初回スキャン中に到着した監視イベントは
//! キューへ蓄積され、スキャン完了後に適用される (バッファリング)。
//! ワーカーはアイドル時にメンテナンス (watcher再確立・劣化ポーリング) を
//! 実行する。

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gento_core::library::{
    hash_and_link_file, scan_root, FsChange, GenerationToken, HashFileError, PendingHash, ScanFs,
    ScanOptions, ScanPhase, ScanProgress, WatchBuffer,
};
use gento_core::repo::{FileRepo, LibraryRootRecord, LibraryRootRepo, RepoError, RootSettings};
use gento_core::Priority;
use gento_image::fs::PlatformFs;
use gento_image::pool::{Job, JobCompletion, PoolConfig, SubmitError, SubmitOutcome, WorkerPool};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use serde::Serialize;
use tauri::{Manager, State};

use crate::catalog::{AppState, ScanProgressPayload};

/// 監視イベントのdebounceウィンドウ (この間イベントを結合する)。
const WATCH_DEBOUNCE_MS: u64 = 400;

/// debounceスレッドのポーリング間隔。
const WATCH_TICK_MS: u64 = 50;

/// スコープ差分のマージ上限。超過時は全体スキャンへ切り替える
/// (大量イベント後の収束を1回のwalkへ集約する)。
const MAX_MERGED_SCOPES: usize = 24;

/// ハッシュ進捗イベントの最小間隔 (イベント嵐対策)。
const HASH_PROGRESS_INTERVAL_MS: u64 = 100;

/// メンテナンス周期 (watcher再確立・劣化ポーリング再スキャン・
/// 未リンクpresentファイルのハッシュ再提出)。
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(60);

/// ハッシュジョブ1ファイルあたりのStale再試行上限 (FG-WP2-2)。
/// 到達したファイルは断念し `scan_status.hash_retry_exhausted` へ露出。
/// 次のスキャンでの再キューイング (新世代トークン) で解除される。
const HASH_RETRY_MAX_ATTEMPTS: u32 = 8;

/// 再提出時のキュー空きマージン (capacity - depth がこれ未満なら再提出しない)。
const HASH_RESUBMIT_MARGIN: usize = 64;

/// 1回の再提出で纏めて取り出すアイテム数の上限。
const HASH_RESUBMIT_BATCH: usize = 256;

/// ライブラリ側からUIへの通知境界 (本実装: AppHandle。テスト: 記録用)。
pub trait LibraryHost: Send + Sync {
    /// `scan:progress` イベント (ペイロード拡張は api-contract §17)。
    fn scan_progress(&self, payload: ScanProgressPayload);
    /// `roots:changed` イベント (ルート一覧の再取得要求)。
    fn roots_changed(&self);
    /// ライブラリスナップショットが変化した (カタログへ反映要求)。
    fn library_snapshot_changed(&self);
}

/// AppHandle経由のイベント送出。
pub struct TauriLibraryHost {
    pub app: tauri::AppHandle,
}

impl LibraryHost for TauriLibraryHost {
    fn scan_progress(&self, payload: ScanProgressPayload) {
        use tauri::Emitter;
        let _ = self.app.emit("scan:progress", payload);
    }

    fn roots_changed(&self) {
        use tauri::Emitter;
        let _ = self.app.emit("roots:changed", ());
    }

    fn library_snapshot_changed(&self) {
        let app = &self.app;
        // スナップショット再生成 → カタログへ反映 (Libraryモード表示中なら
        // generation更新 + catalog:changed)。setupのmanage前に初回スキャンが
        // 終わり得る競合では何もしない (次の更新で追いつく)。
        let Some(state) = app.try_state::<AppState>() else {
            return;
        };
        let snapshot = state
            .library
            .as_ref()
            .and_then(|controller| controller.build_snapshot().ok());
        if let Some(records) = snapshot {
            crate::catalog::apply_library_snapshot(app, records);
        }
    }
}

/// ワーカーへのコマンド。
enum LibCmd {
    /// 全有効ルートの全体スキャン。
    ScanAll,
    /// 指定ルートの全体スキャン (追加・有効化時)。
    ScanRoot(i64),
    /// スコープ差分スキャン (監視イベント適用)。
    ScanScope {
        root_id: i64,
        scope: PathBuf,
    },
    Shutdown,
}

/// ハッシュジョブの決定論リトライ状態 (FG-WP2-2)。
///
/// queue満杯で拒否されたジョブとStale破棄されたファイルは `queue` へ入り、
/// (a) プール完了による空き増加、(b) メンテナンス周期 (未リンクpresent
/// ファイルの再提出) で決定論的に再提出される。Staleの試行回数は
/// ファイルごとに数え、上限到達で断念 (status露出。次のスキャンで解除)。
#[derive(Default)]
struct HashRetry {
    /// 再提出待ちアイテム。
    queue: VecDeque<PendingHash>,
    /// ファイルごとのStale試行回数。
    attempts: HashMap<i64, u32>,
    /// 断念済みfile_id (次のスキャンでのキューイングで解除)。
    exhausted: HashSet<i64>,
}

impl HashRetry {
    fn push(&mut self, item: PendingHash) {
        if !self.exhausted.contains(&item.file_id) {
            self.queue.push_back(item);
        }
    }

    fn note_stale(&mut self, file_id: i64) -> bool {
        let attempts = self.attempts.entry(file_id).or_insert(0);
        *attempts += 1;
        if *attempts >= HASH_RETRY_MAX_ATTEMPTS {
            self.exhausted.insert(file_id);
            self.queue.retain(|item| item.file_id != file_id);
            return false;
        }
        true
    }

    /// スキャン由来の新規キューイングで再試行状態をリセットする
    /// (新世代トークンのジョブは新規扱い)。
    fn reset(&mut self, file_id: i64) {
        self.attempts.remove(&file_id);
        self.exhausted.remove(&file_id);
    }
}

/// 共有状態 (ハッシュジョブからも参照される)。
struct Shared {
    db: Arc<gento_db::Db>,
    fs: Arc<dyn ScanFs>,
    host: Arc<dyn LibraryHost>,
    cancel: AtomicBool,
    /// スキャン位相・進捗 (status commandとイベント送出の正本)。
    progress: Mutex<LibraryProgress>,
    /// ハッシュ待ち残数 (おおよ其の値: キュー投入と完了の差)。
    hash_pending: AtomicU64,
    last_hash_emit: Mutex<Option<Instant>>,
    /// ワーカーがスキャン処理中 (scan_startの二重開始拒否・status表示)。
    scanning: AtomicBool,
    /// ハッシュジョブの決定論リトライ状態 (FG-WP2-2)。
    hash_retry: Mutex<HashRetry>,
    /// ハッシュプールのキュー上限 (再提出の空き判定に使用)。
    hash_queue_capacity: usize,
}

#[derive(Default, Clone, Copy)]
struct LibraryProgress {
    phase: Option<ScanPhase>,
    root_id: i64,
    found: u64,
    updated: u64,
    relocated: u64,
    missing: u64,
    hashed: u64,
    errors: u64,
    entries: u64,
}

/// 監視中ルートの状態。
struct WatchState {
    /// notifyウォッチャー (dropで監視解除)。
    _watcher: notify::RecommendedWatcher,
    debouncer: Option<std::thread::JoinHandle<()>>,
    stop: Arc<AtomicBool>,
}

/// ルートごとの監視状態表 (controller・worker・debouncerスレッドで共有)。
type Watchers = Arc<Mutex<HashMap<i64, WatchState>>>;

/// 登録ルート制御中枢。
pub struct LibraryController {
    shared: Arc<Shared>,
    tx: Sender<LibCmd>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// ハッシュ会計スレッド (プール完了通知でpendingを減算)。
    hash_accountant: Mutex<Option<std::thread::JoinHandle<()>>>,
    watchers: Watchers,
}

impl LibraryController {
    /// コントローラを作成しワーカースレッドを起動する。
    pub fn new(db: Arc<gento_db::Db>, host: Arc<dyn LibraryHost>) -> Result<Arc<Self>, RepoError> {
        Self::with_fs(db, host, Arc::new(PlatformFs))
    }

    /// テスト用: ScanFsを注入する。
    pub fn with_fs(
        db: Arc<gento_db::Db>,
        host: Arc<dyn LibraryHost>,
        fs: Arc<dyn ScanFs>,
    ) -> Result<Arc<Self>, RepoError> {
        Self::with_fs_maintenance(db, host, fs, MAINTENANCE_INTERVAL, 8_192)
    }

    /// テスト用: メンテナンス周期・ハッシュキュー上限も指定する。
    fn with_fs_maintenance(
        db: Arc<gento_db::Db>,
        host: Arc<dyn LibraryHost>,
        fs: Arc<dyn ScanFs>,
        maintenance_interval: Duration,
        hash_queue_capacity: usize,
    ) -> Result<Arc<Self>, RepoError> {
        let (tx, rx) = mpsc::channel::<LibCmd>();
        let (hash_pool, completions) = WorkerPool::new(PoolConfig {
            worker_count: 2,
            queue_capacity: hash_queue_capacity,
        });
        let shared = Arc::new(Shared {
            db,
            fs,
            host,
            cancel: AtomicBool::new(false),
            progress: Mutex::new(LibraryProgress::default()),
            hash_pending: AtomicU64::new(0),
            last_hash_emit: Mutex::new(None),
            scanning: AtomicBool::new(false),
            hash_retry: Mutex::new(HashRetry::default()),
            hash_queue_capacity,
        });
        // ハッシュ会計: submit成功 (Enqueued) 回数とプール完了通知を1:1で
        // 対応させ、pending_hash が恒久非ゼロにならないことを保証する
        // (dedup統合・queue満杯・キャンセル・shutdown放棄を含む)。
        // 完了ごとに再提出キューの空き駆動排液も行う (FG-WP2-2)。
        let accountant =
            spawn_hash_accountant(Arc::clone(&shared), completions, Arc::downgrade(&hash_pool));
        let watchers: Watchers = Arc::new(Mutex::new(HashMap::new()));
        let worker_shared = Arc::clone(&shared);
        let worker_pool = hash_pool;
        let worker_watchers = Arc::clone(&watchers);
        let worker_tx = tx.clone();
        let worker = std::thread::Builder::new()
            .name("library-worker".into())
            .spawn(move || {
                worker_loop(
                    worker_shared,
                    worker_pool,
                    rx,
                    worker_watchers,
                    worker_tx,
                    maintenance_interval,
                )
            })
            .map_err(|e| RepoError::Db(format!("spawn library worker: {e}")))?;
        Ok(Arc::new(Self {
            shared,
            tx,
            worker: Mutex::new(Some(worker)),
            hash_accountant: Mutex::new(Some(accountant)),
            watchers,
        }))
    }

    pub fn db(&self) -> &Arc<gento_db::Db> {
        &self.shared.db
    }

    /// 全有効ルートのスキャンを投入。
    pub fn enqueue_scan_all(&self) {
        let _ = self.tx.send(LibCmd::ScanAll);
    }

    fn enqueue_scan_root(&self, root_id: i64) {
        let _ = self.tx.send(LibCmd::ScanRoot(root_id));
    }

    /// 現在実行中のスキャン・ハッシュをキャンセルする (キューは残す)。
    pub fn cancel_scan(&self) {
        self.shared.cancel.store(true, Ordering::SeqCst);
    }

    pub fn is_scanning(&self) -> bool {
        self.shared.scanning.load(Ordering::SeqCst)
    }

    /// DBから現在有効なルートを列挙 (available = パスがディレクトリ存在)。
    pub fn list_roots(&self) -> Result<Vec<(LibraryRootRecord, bool)>, RepoError> {
        let roots = self.shared.db.with_reader(|repos| repos.list_roots())?;
        Ok(roots
            .into_iter()
            .map(|root| {
                let available = root.path.is_dir();
                (root, available)
            })
            .collect())
    }

    /// ルートを追加 (存在確認・親子重複警告)。スキャンと監視を開始する。
    pub fn add_root(
        &self,
        path: &Path,
    ) -> Result<(LibraryRootRecord, Vec<LibraryRootRecord>), String> {
        let canonical = std::fs::canonicalize(path)
            .map_err(|e| format!("path does not exist or is not accessible: {e}"))?;
        if !canonical.is_dir() {
            return Err("path is not a directory".to_string());
        }
        let overlapping = self
            .shared
            .db
            .with_reader(|repos| repos.overlapping_roots(&canonical))
            .map_err(|e| e.to_string())?;
        if overlapping.iter().any(|root| root.path == canonical) {
            return Err("path is already registered".to_string());
        }
        let root = self
            .shared
            .db
            .with_writer(|repos| repos.add_root(&canonical))
            .map_err(|e| e.to_string())?;
        // 既定設定 (watch=on) を明示 (schema既定と同一)。
        let _ = self
            .shared
            .db
            .with_writer(|repos| repos.set_root_settings(root.id, RootSettings::default()));
        if let Err(err) = self.start_root_watcher(&root) {
            tracing::warn!(root_id = root.id, error = %err, "watcher unavailable for root");
        }
        self.enqueue_scan_root(root.id);
        self.shared.host.roots_changed();
        Ok((root, overlapping))
    }

    /// ルートを解除 (配下FileはCASCADE削除。Assetは残る)。
    pub fn remove_root(&self, root_id: i64) -> Result<(), String> {
        self.stop_watcher(root_id);
        self.shared
            .db
            .with_writer(|repos| repos.remove_root(root_id))
            .map_err(|e| e.to_string())?;
        self.shared.host.roots_changed();
        self.shared.host.library_snapshot_changed();
        Ok(())
    }

    /// 有効状態を切り替える。無効化で監視停止、有効化で監視とスキャン再開。
    pub fn set_root_enabled(&self, root_id: i64, enabled: bool) -> Result<(), String> {
        self.shared
            .db
            .with_writer(|repos| repos.set_root_enabled(root_id, enabled))
            .map_err(|e| e.to_string())?;
        if enabled {
            let roots = self.list_roots().map_err(|e| e.to_string())?;
            if let Some((root, available)) = roots.iter().find(|(r, _)| r.id == root_id) {
                if *available && root.watch_enabled {
                    let _ = self.start_root_watcher(root);
                }
                self.enqueue_scan_root(root_id);
            }
        } else {
            self.stop_watcher(root_id);
        }
        self.shared.host.roots_changed();
        Ok(())
    }

    /// ライブラリカタログのスナップショットをDBから生成する。
    pub fn build_snapshot(&self) -> Result<Vec<gento_core::catalog::ImageRecord>, RepoError> {
        build_library_snapshot(&self.shared.db)
    }

    /// ルートの監視を開始する (setup・有効化からも呼ぶ公開ラッパ)。
    pub fn start_root_watcher(&self, root: &LibraryRootRecord) -> Result<(), String> {
        start_watcher(&self.watchers, &self.shared, &self.tx, root)
    }

    /// ルートの監視を停止する。
    fn stop_watcher(&self, root_id: i64) {
        stop_watcher(&self.watchers, root_id);
    }

    /// 監視を停止しワーカー・ハッシュプールを終了する (アプリ終了時)。
    pub fn shutdown(&self) {
        let ids: Vec<i64> = self
            .watchers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .keys()
            .copied()
            .collect();
        for id in ids {
            self.stop_watcher(id);
        }
        self.cancel_scan();
        let _ = self.tx.send(LibCmd::Shutdown);
        if let Some(handle) = self.worker.lock().unwrap_or_else(|p| p.into_inner()).take() {
            let _ = handle.join();
        }
        // workerのShutdown処理内でpoolがshutdownされ、完了通知 (放棄分の
        // Cancelledを含む) が会計スレッドへ流れる。poolのArcが全dropされ
        // チャネルが閉じた時点で会計スレッドも終了する。
        if let Some(handle) = self
            .hash_accountant
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            let _ = handle.join();
        }
    }

    fn status_dto(&self) -> LibraryScanStatusDto {
        let progress = *self
            .shared
            .progress
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let pending = self.shared.hash_pending.load(Ordering::SeqCst);
        let (retry_pending, retry_exhausted) = {
            let retry = self
                .shared
                .hash_retry
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            (retry.queue.len() as u64, retry.exhausted.len() as u64)
        };
        let phase = if self.is_scanning() {
            Some(ScanPhase::Scanning.as_str().to_string())
        } else if pending > 0 {
            Some(ScanPhase::Hashing.as_str().to_string())
        } else {
            None
        };
        LibraryScanStatusDto {
            scanning: self.is_scanning(),
            phase,
            found: progress.found,
            updated: progress.updated,
            relocated: progress.relocated,
            missing: progress.missing,
            hashed: progress.hashed,
            pending_hash: pending,
            errors: progress.errors,
            watch_degraded: self.watch_degraded(),
            hash_retry_pending: retry_pending,
            hash_retry_exhausted: retry_exhausted,
        }
    }

    /// watcher未確立の有効ルートがあるか (inotify上限等の劣化表示)。
    fn watch_degraded(&self) -> bool {
        let Ok(roots) = self.list_roots() else {
            return false;
        };
        let watched: std::collections::HashSet<i64> = self
            .watchers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .keys()
            .copied()
            .collect();
        roots.into_iter().any(|(root, available)| {
            root.enabled && root.watch_enabled && available && !watched.contains(&root.id)
        })
    }
}

/// ルートの監視を開始する (controller各所・メンテナンスから呼ぶ)。
fn start_watcher(
    watchers: &Watchers,
    shared: &Arc<Shared>,
    tx: &Sender<LibCmd>,
    root: &LibraryRootRecord,
) -> Result<(), String> {
    let mut guard = watchers.lock().unwrap_or_else(|p| p.into_inner());
    if guard.contains_key(&root.id) {
        return Ok(());
    }
    let buffer = Arc::new(WatchBuffer::new());
    let (event_tx, event_rx) = mpsc::channel::<(FsChange, PathBuf)>();
    let notify_buffer = Arc::clone(&buffer);
    let root_path = root.path.clone();
    let mut watcher =
        notify::recommended_watcher(move |result: Result<Event, notify::Error>| match result {
            Ok(event) => {
                for path in &event.paths {
                    let change = match event.kind {
                        EventKind::Create(_) => Some(FsChange::Created),
                        EventKind::Modify(_) => Some(FsChange::Modified),
                        EventKind::Remove(_) => Some(FsChange::Removed),
                        EventKind::Access(_) => None,
                        // 未知種 (Any/Other・inotify overflow等) は保守的に
                        // 全体再スキャンへ逃がす
                        _ => {
                            notify_buffer.mark_overflow();
                            None
                        }
                    };
                    if let Some(change) = change {
                        notify_buffer.push(change, path.clone());
                        let _ = event_tx.send((change, path.clone()));
                    }
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, "watch error (treated as overflow)");
                notify_buffer.mark_overflow();
            }
        })
        .map_err(|e| e.to_string())?;
    watcher
        .watch(&root.path, RecursiveMode::Recursive)
        .map_err(|e| e.to_string())?;

    let stop = Arc::new(AtomicBool::new(false));
    let debouncer = spawn_debouncer(DebouncerParams {
        buffer,
        event_rx,
        stop: Arc::clone(&stop),
        root: root_path,
        root_id: root.id,
        shared: Arc::clone(shared),
        cmd_tx: tx.clone(),
        watchers: Arc::clone(watchers),
    });
    guard.insert(
        root.id,
        WatchState {
            _watcher: watcher,
            debouncer: Some(debouncer),
            stop,
        },
    );
    Ok(())
}

/// ルートの監視を停止する。停止フラグの設定とjoinを分離し、debouncer側の
/// 自己登録解除 (watchersロック取得) とデッドロックしないようにする。
fn stop_watcher(watchers: &Watchers, root_id: i64) {
    let state = watchers
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(&root_id);
    // watcherはここでdropされinotify監視が解除される
    if let Some(mut state) = state {
        state.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = state.debouncer.take() {
            let _ = handle.join();
        }
    }
}

/// メンテナンス (ワーカーのアイドル時に周期的に実行):
/// - 不在ルートのwatcherを解放 (イベント欠落したアンマウントの掃除)
/// - 再出現・watcher確立失敗からの復帰 (再確立できたら再スキャンを投入)
/// - 確立できない有効ルートはポーリング再スキャンで劣化運用 (degraded)
///
/// `try_start` はwatcher確立の試行 (テストで失敗を注入できるよう分離)。
fn run_maintenance(
    shared: &Arc<Shared>,
    watchers: &Watchers,
    tx: &Sender<LibCmd>,
    try_start: &dyn Fn(&LibraryRootRecord) -> Result<(), String>,
) {
    let Ok(roots) = shared.db.with_reader(|repos| repos.list_roots()) else {
        return;
    };
    for root in roots.iter().filter(|root| root.enabled) {
        let available = root.path.is_dir();
        let watched = watchers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(&root.id);
        if !available {
            if watched {
                tracing::info!(root_id = root.id, "root unavailable; releasing watcher");
                stop_watcher(watchers, root.id);
                shared.host.roots_changed();
            }
            continue;
        }
        if root.watch_enabled && !watched {
            match try_start(root) {
                Ok(()) => {
                    tracing::info!(root_id = root.id, "watcher re-established");
                    let _ = tx.send(LibCmd::ScanRoot(root.id));
                }
                Err(err) => {
                    tracing::warn!(
                        root_id = root.id,
                        error = %err,
                        "watcher unavailable; degraded polling rescan"
                    );
                    let _ = tx.send(LibCmd::ScanRoot(root.id));
                }
            }
        }
    }
}

/// debounceスレッドの起動引数。
struct DebouncerParams {
    buffer: Arc<WatchBuffer>,
    event_rx: Receiver<(FsChange, PathBuf)>,
    stop: Arc<AtomicBool>,
    root: PathBuf,
    root_id: i64,
    shared: Arc<Shared>,
    cmd_tx: Sender<LibCmd>,
    watchers: Watchers,
}

/// debounceスレッド: ウィンドウ内のイベントを結合しスコープ差分へ変換する。
fn spawn_debouncer(params: DebouncerParams) -> std::thread::JoinHandle<()> {
    let DebouncerParams {
        buffer,
        event_rx,
        stop,
        root,
        root_id,
        shared,
        cmd_tx,
        watchers,
    } = params;
    std::thread::Builder::new()
        .name(format!("watch-debounce-{root_id}"))
        .spawn(move || {
            let debounce = Duration::from_millis(WATCH_DEBOUNCE_MS);
            let mut last_event = None::<Instant>;
            loop {
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                match event_rx.recv_timeout(Duration::from_millis(WATCH_TICK_MS)) {
                    Ok(_) => last_event = Some(Instant::now()),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
                // 通常イベントが無くてもoverflow (watcherエラー・未知イベント種
                // を含む) は50ms tickで検出し、全体再スキャンへ収束させる。
                let last = match last_event {
                    Some(last) => last,
                    None => {
                        if buffer.is_overflow() {
                            Instant::now()
                        } else {
                            continue;
                        }
                    }
                };
                if last.elapsed() < debounce && !buffer.is_overflow() {
                    continue;
                }
                last_event = None;
                let coalesced = buffer.drain();
                if coalesced.changes.is_empty() && !coalesced.overflow {
                    continue;
                }
                // ルート自体の削除 (アンマウント含む): watcherを解放して終了。
                // 自エントリを外すことで、再出現時にメンテナンスが再確立できる。
                // (stop_watcherはjoinするため自己登録解除には使えない —
                //  自己joinになる。ここではロックのみで外し、watcherのdropで
                //  inotifyを解放する。joinハンドルはdetachされる)
                if coalesced
                    .changes
                    .get(&root)
                    .is_some_and(|kind| *kind == FsChange::Removed)
                {
                    shared.host.roots_changed();
                    tracing::info!(root_id, "root removed; releasing watcher");
                    watchers
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .remove(&root_id);
                    return;
                }
                if coalesced.overflow {
                    tracing::warn!(root_id, "watch overflow; scheduling full rescan");
                    let _ = cmd_tx.send(LibCmd::ScanRoot(root_id));
                    continue;
                }
                // 変化パス → 影響ディレクトリ (ルート相対)。
                let mut scopes: BTreeSet<PathBuf> = BTreeSet::new();
                let mut full = false;
                for path in coalesced.changes.keys() {
                    match path.strip_prefix(&root) {
                        Ok(rel) if rel.as_os_str().is_empty() => {
                            full = true;
                            break;
                        }
                        Ok(rel) => {
                            let parent = rel.parent().unwrap_or(Path::new(""));
                            scopes.insert(parent.to_path_buf());
                        }
                        Err(_) => {
                            full = true;
                            break;
                        }
                    }
                }
                if full {
                    let _ = cmd_tx.send(LibCmd::ScanRoot(root_id));
                } else {
                    for scope in scopes {
                        let _ = cmd_tx.send(LibCmd::ScanScope { root_id, scope });
                    }
                }
            }
        })
        .expect("spawn watch debouncer")
}

/// 単一ワーカーループ。コマンドを逐次実行し、スキャン中に到着した
/// スコープコマンドを次周でマージする。アイドルがメンテナンス周期に
/// 達したらwatcher再確立・劣化ポーリングを実行する。
fn worker_loop(
    shared: Arc<Shared>,
    hash_pool: Arc<WorkerPool>,
    rx: Receiver<LibCmd>,
    watchers: Watchers,
    tx: Sender<LibCmd>,
    maintenance_interval: Duration,
) {
    loop {
        let cmd = match rx.recv_timeout(maintenance_interval) {
            Ok(cmd) => cmd,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let shared_ref = &shared;
                let watchers_ref = &watchers;
                let tx_ref = &tx;
                run_maintenance(shared_ref, watchers_ref, tx_ref, &|root| {
                    start_watcher(watchers_ref, shared_ref, tx_ref, root)
                });
                // 未リンクpresentファイルの決定論的再提出 (FG-WP2-2)。
                // 監視イベントがない不変ファイルもここで解決される。
                resubmit_unlinked_via_maintenance(shared_ref, &hash_pool);
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        match cmd {
            LibCmd::Shutdown => {
                hash_pool.shutdown();
                return;
            }
            LibCmd::ScanAll => {
                run_scan_all(&shared, &hash_pool, None);
            }
            LibCmd::ScanRoot(root_id) => {
                run_scan_all(&shared, &hash_pool, Some(root_id));
            }
            LibCmd::ScanScope { root_id, scope } => {
                // 追い越しで溜まったスコープをマージ (多すぎたら全体へ)。
                let mut scopes: BTreeSet<PathBuf> = BTreeSet::new();
                scopes.insert(scope);
                while let Ok(next) = rx.try_recv() {
                    match next {
                        LibCmd::ScanScope {
                            root_id: next_root,
                            scope: next_scope,
                        } => {
                            if next_root == root_id {
                                scopes.insert(next_scope);
                            } else {
                                run_scan_all(&shared, &hash_pool, Some(next_root));
                            }
                        }
                        LibCmd::ScanAll => {
                            run_scan_all(&shared, &hash_pool, None);
                            scopes.clear();
                            break;
                        }
                        LibCmd::ScanRoot(other) => {
                            run_scan_all(&shared, &hash_pool, Some(other));
                        }
                        LibCmd::Shutdown => return,
                    }
                    if scopes.len() > MAX_MERGED_SCOPES {
                        break;
                    }
                }
                if !scopes.is_empty() {
                    if scopes.len() > MAX_MERGED_SCOPES {
                        run_scan_all(&shared, &hash_pool, Some(root_id));
                    } else {
                        for scope in scopes {
                            run_scan_root(&shared, &hash_pool, root_id, Some(&scope));
                        }
                    }
                }
            }
        }
    }
}

fn run_scan_all(shared: &Arc<Shared>, hash_pool: &Arc<WorkerPool>, only_root: Option<i64>) {
    shared.scanning.store(true, Ordering::SeqCst);
    run_scan_all_inner(shared, hash_pool, only_root);
    shared.scanning.store(false, Ordering::SeqCst);
}

fn run_scan_all_inner(shared: &Arc<Shared>, hash_pool: &Arc<WorkerPool>, only_root: Option<i64>) {
    let roots = match shared.db.with_reader(|repos| repos.list_roots()) {
        Ok(roots) => roots
            .into_iter()
            .filter(|root| root.enabled && only_root.is_none_or(|id| root.id == id))
            .collect::<Vec<_>>(),
        Err(err) => {
            tracing::error!(error = %err, "list roots failed");
            return;
        }
    };
    for root in roots {
        run_scan_root(shared, hash_pool, root.id, None);
    }
}

fn run_scan_root(
    shared: &Arc<Shared>,
    hash_pool: &Arc<WorkerPool>,
    root_id: i64,
    scope: Option<&Path>,
) {
    let root = match shared.db.with_reader(|repos| repos.list_roots()) {
        Ok(roots) => match roots.into_iter().find(|root| root.id == root_id) {
            Some(root) => root,
            None => return,
        },
        Err(err) => {
            tracing::error!(root_id, error = %err, "load root failed");
            return;
        }
    };
    if !root.path.is_dir() {
        // 不在ルート: スキャンせず表示だけ無効化 (DBは保持)
        tracing::info!(root_id, path = %root.path.display(), "root unavailable; skip scan");
        shared.host.roots_changed();
        return;
    }

    shared.cancel.store(false, Ordering::SeqCst);
    let host = Arc::clone(&shared.host);
    let started = Instant::now();
    let mut error_samples: Vec<String> = Vec::new();
    let summary = scan_root(
        &*shared.db,
        &*shared.fs,
        &root,
        scope,
        &shared.cancel,
        &ScanOptions::default(),
        &mut |progress: ScanProgress| {
            update_progress(shared, |current| {
                current.phase = Some(ScanPhase::Scanning);
                current.root_id = progress.root_id;
                current.entries = progress.entries_walked;
                current.found = progress.found;
                current.updated = progress.updated;
                current.relocated = progress.relocated;
                current.missing = progress.missing;
                current.errors = progress.errors;
            });
            host.scan_progress(progress_payload(shared, started, false, &[]));
        },
    );
    match summary {
        Ok(summary) => {
            error_samples = summary.error_samples.clone();
            enqueue_hash_jobs(shared, hash_pool, summary.pending_hash);
            if summary.cancelled {
                tracing::info!(root_id, "scan cancelled");
            }
        }
        Err(err) => {
            tracing::error!(root_id, error = %err, "scan failed");
            error_samples.push(err.to_string());
            update_progress(shared, |current| current.errors += 1);
        }
    }
    // スキャン結果をカタログスナップショットへ反映 (走査中もページ取得可能な
    // まま、ここで一括更新。ハッシュはこの後Backgroundで追いつく)。
    shared.host.library_snapshot_changed();
    let done = shared.hash_pending.load(Ordering::SeqCst) == 0;
    host.scan_progress(progress_payload(shared, started, done, &error_samples));
}

fn update_progress(shared: &Arc<Shared>, f: impl FnOnce(&mut LibraryProgress)) {
    let mut progress = shared.progress.lock().unwrap_or_else(|p| p.into_inner());
    f(&mut progress);
}

fn progress_payload(
    shared: &Arc<Shared>,
    started: Instant,
    done: bool,
    error_samples: &[String],
) -> ScanProgressPayload {
    let progress = *shared.progress.lock().unwrap_or_else(|p| p.into_inner());
    ScanProgressPayload {
        scanned: progress.entries,
        found: progress.found,
        errors: progress.errors,
        elapsed_ms: started.elapsed().as_millis() as u64,
        done,
        error_samples: error_samples.to_vec(),
        updated: progress.updated,
        relocated: progress.relocated,
        missing: progress.missing,
        hashed: progress.hashed,
        pending_hash: shared.hash_pending.load(Ordering::SeqCst),
        phase: progress.phase.map(|p| p.as_str().to_string()),
    }
}

/// ハッシュ会計スレッド: プールの完了通知 (実行完了・失敗・キャンセル・
/// shutdown放棄のすべて) を `hash_pending` の減算と1:1で対応させる。
/// これによりpendingは「submit成功数 − 完了通知数」に厳密に一致し、
/// dedup統合・queue満杯・キャンセル・pool shutdownのいずれでも
/// 恒久非ゼロにならない。
/// 各完了ごとに再提出キューの空き駆動排液も行う (FG-WP2-2: プール滞留の
/// 減少 = 空きを見た決定論的再提出)。
fn spawn_hash_accountant(
    shared: Arc<Shared>,
    completions: Receiver<JobCompletion>,
    pool: std::sync::Weak<WorkerPool>,
) -> std::thread::JoinHandle<()> {
    // プールはWeak参照でのみ持つ: 強参照だと会計スレッド自身が完了チャネルの
    // 送信側を生存させ続け、recvが永久に閉じずjoinでデッドロックする。
    std::thread::Builder::new()
        .name("hash-accountant".into())
        .spawn(move || {
            while let Ok(completion) = completions.recv() {
                if completion.key.starts_with("hash:") {
                    shared.hash_pending.fetch_sub(1, Ordering::SeqCst);
                    if let Some(pool) = pool.upgrade() {
                        resubmit_ready(&shared, &pool);
                    }
                }
            }
        })
        .expect("spawn hash accountant")
}

/// 再提出キューから空き容量ぶんだけプールへ戻す (完了駆動)。
/// 1完了につき1回だけ試み、満杯なら残りはキューに戻る (次の完了で再試行)。
fn resubmit_ready(shared: &Arc<Shared>, pool: &Arc<WorkerPool>) {
    if shared.cancel.load(Ordering::SeqCst) {
        return;
    }
    // マージンは容量の1/4までに縮める (小容量キューで再提出が恒久的に
    // 起きなくならないように)。
    let margin = HASH_RESUBMIT_MARGIN.min(shared.hash_queue_capacity / 4);
    let free = shared
        .hash_queue_capacity
        .saturating_sub(pool.queue_depth())
        .saturating_sub(margin);
    if free == 0 {
        return;
    }
    let items: Vec<PendingHash> = {
        let mut retry = shared.hash_retry.lock().unwrap_or_else(|p| p.into_inner());
        let take = free.min(HASH_RESUBMIT_BATCH).min(retry.queue.len());
        retry.queue.drain(..take).collect()
    };
    if items.is_empty() {
        return;
    }
    submit_hash_jobs(shared, pool, items, false);
}

/// メンテナンス周期での未リンクpresentファイル再提出 (FG-WP2-2)。
/// 監視イベントが来ない不変ファイルも、ここで「永続未解決」にならない。
/// 断念済み (exhausted) ファイルは次のスキャンでの再キューイングまで跳ばす。
fn resubmit_unlinked_via_maintenance(shared: &Arc<Shared>, pool: &Arc<WorkerPool>) {
    if shared.cancel.load(Ordering::SeqCst) {
        return;
    }
    let Ok(roots) = shared.db.with_reader(|repos| repos.list_roots()) else {
        return;
    };
    for root in roots.iter().filter(|root| root.enabled) {
        let Ok(unlinked) = shared
            .db
            .with_reader(|repos| repos.list_files_without_asset(root.id))
        else {
            continue;
        };
        let items: Vec<PendingHash> = {
            let retry = shared.hash_retry.lock().unwrap_or_else(|p| p.into_inner());
            unlinked
                .into_iter()
                .filter(|record| !retry.exhausted.contains(&record.id))
                .map(|record| PendingHash {
                    file_id: record.id,
                    path: root.path.join(&record.rel_path),
                    token: GenerationToken::from_record(&record),
                })
                .collect()
        };
        submit_hash_jobs(shared, pool, items, false);
    }
}

/// Stale破棄されたジョブの再提出登録 (FG-WP2-2)。
/// 行の現在トークンを読み直して次回試行に使い、リンク済み・不在・削除済み
/// は再提出しない。試行回数が上限に達したファイルは断念する。
fn requeue_after_stale(shared: &Arc<Shared>, file_id: i64) {
    let still_retryable = shared
        .hash_retry
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .note_stale(file_id);
    if !still_retryable {
        tracing::warn!(
            file_id,
            attempts = HASH_RETRY_MAX_ATTEMPTS,
            "hash retries exhausted; awaiting next scan"
        );
        return;
    }
    let Ok(record) = shared
        .db
        .with_reader(|repos| repos.get_file(file_id))
        .map_err(|err| {
            tracing::error!(file_id, error = %err, "requeue: get_file failed");
            err
        })
    else {
        return;
    };
    let Some(record) = record else { return };
    if record.asset_id.is_some() || record.scan_state != gento_core::repo::ScanState::Present {
        return;
    }
    let Ok(roots) = shared.db.with_reader(|repos| repos.list_roots()) else {
        return;
    };
    let Some(root) = roots.iter().find(|root| root.id == record.root_id) else {
        return;
    };
    let item = PendingHash {
        file_id,
        path: root.path.join(&record.rel_path),
        token: GenerationToken::from_record(&record),
    };
    shared
        .hash_retry
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(item);
}

/// ハッシュジョブ (Background優先度)。完了でhashed進捗を進める。
/// `hash_pending` の減算は会計スレッド (完了通知ドリブン) が行うため、
/// ここでは進捗・通知のみを担う。
struct HashJob {
    shared: Arc<Shared>,
    file_id: i64,
    path: PathBuf,
    /// 走査時観測の世代トークン (FG-WP2-2)。
    token: GenerationToken,
}

impl Job for HashJob {
    fn run(&self, _cancel: &gento_image::pool::CancelToken) -> Result<Vec<u8>, String> {
        if self.shared.cancel.load(Ordering::SeqCst) {
            return Err("cancelled".to_string());
        }
        match hash_and_link_file(
            &*self.shared.db,
            &*self.shared.fs,
            self.file_id,
            &self.path,
            &self.token,
            &self.shared.cancel,
        ) {
            Ok(_asset) => {
                let hashed = {
                    update_progress(&self.shared, |current| current.hashed += 1);
                    self.shared
                        .progress
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .hashed
                };
                // スナップショット反映は一定間隔で (毎ファイルはイベント嵐)。
                let should_emit = {
                    let mut last = self
                        .shared
                        .last_hash_emit
                        .lock()
                        .unwrap_or_else(|p| p.into_inner());
                    let due = last
                        .map(|t| t.elapsed().as_millis() as u64 >= HASH_PROGRESS_INTERVAL_MS)
                        .unwrap_or(true);
                    if due {
                        *last = Some(Instant::now());
                    }
                    due
                };
                if should_emit {
                    let remaining = self.shared.hash_pending.load(Ordering::SeqCst);
                    update_progress(&self.shared, |current| {
                        current.phase = Some(if remaining > 0 {
                            ScanPhase::Hashing
                        } else {
                            ScanPhase::Scanning
                        })
                    });
                    self.shared.host.library_snapshot_changed();
                    let done = remaining == 0;
                    self.shared.host.scan_progress(progress_payload(
                        &self.shared,
                        Instant::now(),
                        done,
                        &[],
                    ));
                }
                let _ = hashed;
                Ok(Vec::new())
            }
            // 世代検証で破棄: エラーではなく「要再提出」。決定論リトライ
            // (FG-WP2-2) で行の現在トークンとともに再提出される。監視・
            // メンテナンス・次回スキャンも併せて収束させる。
            Err(HashFileError::Stale(msg)) => {
                tracing::debug!(file_id = self.file_id, reason = %msg, "hash stale; queued for deterministic retry");
                requeue_after_stale(&self.shared, self.file_id);
                Err(format!("stale: {msg}"))
            }
            Err(HashFileError::Cancelled) => Err("cancelled".to_string()),
            Err(err) => {
                update_progress(&self.shared, |current| current.errors += 1);
                Err(err.to_string())
            }
        }
    }
}

/// スキャン由来のハッシュジョブ投入。`hash_pending` は実際の投入成功
/// (新規Enqueued) のみ増やす: dedup統合 (MergedExisting) は元ジョブの
/// 完了通知で減算されるため二重計上しない。満杯で拒否された分と残りは
/// 決定論リトライキューへ移され、プール空き駆動・メンテナンス周期で
/// 再提出される (FG-WP2-2)。スキャン由来のキューイングは再試行回数を
/// リセットする (新世代トークンのジョブは新規扱い)。
fn enqueue_hash_jobs(shared: &Arc<Shared>, hash_pool: &Arc<WorkerPool>, pending: Vec<PendingHash>) {
    submit_hash_jobs(shared, hash_pool, pending, true);
}

/// ジョブ投入の実体。`reset_attempts` はスキャン由来 (新規) の場合のみtrue
/// (リトライ経路でtrueにすると試行上限が機能しなくなる)。
fn submit_hash_jobs(
    shared: &Arc<Shared>,
    hash_pool: &Arc<WorkerPool>,
    pending: Vec<PendingHash>,
    reset_attempts: bool,
) {
    if pending.is_empty() {
        return;
    }
    let mut enqueued = 0u64;
    let mut merged = 0u64;
    let mut rejected = 0u64;
    let mut rest: Vec<PendingHash> = pending;
    for index in 0..rest.len() {
        let item = rest[index].clone();
        if reset_attempts {
            shared
                .hash_retry
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .reset(item.file_id);
        }
        shared.hash_pending.fetch_add(1, Ordering::SeqCst);
        let job = Arc::new(HashJob {
            shared: Arc::clone(shared),
            file_id: item.file_id,
            path: item.path,
            token: item.token,
        });
        let key = format!("hash:{}", item.file_id);
        match hash_pool.submit(&key, Priority::Background, job) {
            Ok(SubmitOutcome::Enqueued(_)) => enqueued += 1,
            Ok(SubmitOutcome::MergedExisting(_)) => {
                // 元ジョブの完了通知1回でこの増分は相殺できない (元の増分しか
                // 無い) ので、ここで戻す。
                shared.hash_pending.fetch_sub(1, Ordering::SeqCst);
                merged += 1;
            }
            Err(err) => {
                shared.hash_pending.fetch_sub(1, Ordering::SeqCst);
                rejected += 1;
                match err {
                    SubmitError::QueueFull => {
                        // バックプレッシャー: 残り (本件含む) は決定論リトライ
                        // キューへ。プール空き駆動・メンテナンス周期で再提出。
                        tracing::warn!(
                            remaining = rest.len() - index,
                            "hash queue full; remaining files queued for deterministic retry"
                        );
                        let queued: Vec<PendingHash> = rest.split_off(index);
                        let mut retry = shared.hash_retry.lock().unwrap_or_else(|p| p.into_inner());
                        for item in queued {
                            retry.push(item);
                        }
                        break;
                    }
                    SubmitError::Shutdown => break,
                }
            }
        }
    }
    tracing::debug!(
        enqueued,
        merged,
        rejected,
        total_pending = shared.hash_pending.load(Ordering::SeqCst),
        "hash jobs enqueued"
    );
}

// ===== カタログスナップショット生成 =====

/// DB (files LEFT JOIN assets) からライブラリカタログのスナップショットを
/// 作る。AssetIdは api-contract §17 の符号化 (assets.id / 仮定ファイルID)。
pub fn build_library_snapshot(
    db: &gento_db::Db,
) -> Result<Vec<gento_core::catalog::ImageRecord>, RepoError> {
    use gento_core::catalog::{ImageRecord, ImageState, ThumbnailState, Triage};
    use gento_core::library::{library_asset_id, provisional_file_asset_id};

    let roots = db.with_reader(|repos| repos.list_roots())?;
    let root_paths: HashMap<i64, PathBuf> = roots
        .iter()
        .map(|root| (root.id, root.path.clone()))
        .collect();
    let joined = db.with_reader(|repos| repos.list_present_joined())?;
    let mut records = Vec::with_capacity(joined.len());
    for (file, asset) in joined {
        let Some(root_path) = root_paths.get(&file.root_id) else {
            continue;
        };
        let (id, width, height, mime, is_animated, rating, triage) = match &asset {
            Some(asset) => (
                library_asset_id(asset.id),
                asset.width,
                asset.height,
                asset.mime_type.clone(),
                asset.is_animated.unwrap_or(false),
                asset.rating,
                match asset.triage {
                    gento_core::repo::TriageState::Unselected => Triage::Undecided,
                    gento_core::repo::TriageState::Picked => Triage::Keep,
                    gento_core::repo::TriageState::Rejected => Triage::Reject,
                },
            ),
            None => (
                provisional_file_asset_id(file.id),
                None,
                None,
                None,
                false,
                0,
                Triage::Undecided,
            ),
        };
        records.push(ImageRecord {
            id,
            source_path: root_path.join(&file.rel_path),
            display_name: file.display_name,
            width,
            height,
            byte_size: file.byte_size,
            mtime_ms: file.mtime_ms,
            mime_type: mime,
            is_animated,
            state: ImageState::Ready,
            thumb_state: ThumbnailState::Missing,
            rating,
            triage,
        });
    }
    records.sort_by(|a, b| {
        a.source_path
            .to_string_lossy()
            .cmp(&b.source_path.to_string_lossy())
    });
    Ok(records)
}

// ===== DTO・command =====

/// `library_roots_list` の項目DTO。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LibraryRootDto {
    /// 10進文字列 (AssetIdの文字列方針に合わせる)。
    pub id: String,
    pub uuid: String,
    pub path: String,
    pub enabled: bool,
    pub watch_enabled: bool,
    /// パスが現在存在するか (アンマウント検出。UIの無効表示用)。
    pub available: bool,
}

fn root_dto(root: &LibraryRootRecord, available: bool) -> LibraryRootDto {
    LibraryRootDto {
        id: root.id.to_string(),
        uuid: root.uuid.clone(),
        path: root.display_path.clone(),
        enabled: root.enabled,
        watch_enabled: root.watch_enabled,
        available,
    }
}

/// `library_roots_add` の応答。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AddRootResponse {
    pub root: LibraryRootDto,
    /// 親子関係にある既存ルート (二重登録警告。登録自体は行う)。
    pub overlapping: Vec<LibraryRootDto>,
}

/// `scan_status` の応答DTO。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LibraryScanStatusDto {
    pub scanning: bool,
    /// `"scanning"` | `"hashing"` | null。
    pub phase: Option<String>,
    pub found: u64,
    pub updated: u64,
    pub relocated: u64,
    pub missing: u64,
    pub hashed: u64,
    pub pending_hash: u64,
    pub errors: u64,
    /// true = watcher未確立の有効ルートがある (inotify上限等の劣化。
    /// メンテナンス周期のポーリング再スキャンで運用継続)。
    pub watch_degraded: bool,
    /// 再提出待ちハッシュジョブ数 (queue満杯拒否・Stale破棄分。
    /// FG-WP2-2)。
    pub hash_retry_pending: u64,
    /// Stale再試行上限に到達し断念したファイル数 (次のスキャンで再試行
    /// 状態はリセットされる)。
    pub hash_retry_exhausted: u64,
}

fn controller(state: &State<'_, AppState>) -> Result<Arc<LibraryController>, String> {
    state
        .library
        .clone()
        .ok_or_else(|| "library feature unavailable (database not open)".to_string())
}

fn parse_root_id(value: &str) -> Result<i64, String> {
    value
        .parse::<i64>()
        .map_err(|_| format!("invalid root id: {value}"))
}

#[tauri::command]
pub fn library_roots_list(state: State<'_, AppState>) -> Result<Vec<LibraryRootDto>, String> {
    let controller = controller(&state)?;
    controller
        .list_roots()
        .map(|roots| {
            roots
                .iter()
                .map(|(root, available)| root_dto(root, *available))
                .collect()
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn library_roots_add(
    state: State<'_, AppState>,
    path: String,
) -> Result<AddRootResponse, String> {
    let controller = controller(&state)?;
    let (root, overlapping) = controller.add_root(Path::new(&path))?;
    Ok(AddRootResponse {
        root: root_dto(&root, true),
        overlapping: overlapping.iter().map(|r| root_dto(r, true)).collect(),
    })
}

#[tauri::command]
pub fn library_roots_remove(state: State<'_, AppState>, root_id: String) -> Result<(), String> {
    let controller = controller(&state)?;
    controller.remove_root(parse_root_id(&root_id)?)
}

#[tauri::command]
pub fn library_roots_set_enabled(
    state: State<'_, AppState>,
    root_id: String,
    enabled: bool,
) -> Result<(), String> {
    let controller = controller(&state)?;
    controller.set_root_enabled(parse_root_id(&root_id)?, enabled)
}

#[tauri::command]
pub fn scan_start(state: State<'_, AppState>) -> Result<(), String> {
    let controller = controller(&state)?;
    if controller.is_scanning() {
        return Err("scan already in progress".to_string());
    }
    controller.enqueue_scan_all();
    Ok(())
}

#[tauri::command]
pub fn scan_stop(state: State<'_, AppState>) -> Result<(), String> {
    let controller = controller(&state)?;
    controller.cancel_scan();
    Ok(())
}

#[tauri::command]
pub fn scan_status(state: State<'_, AppState>) -> LibraryScanStatusDto {
    match state.library.as_ref() {
        Some(controller) => controller.status_dto(),
        None => LibraryScanStatusDto {
            scanning: false,
            phase: None,
            found: 0,
            updated: 0,
            relocated: 0,
            missing: 0,
            hashed: 0,
            pending_hash: 0,
            errors: 0,
            watch_degraded: false,
            hash_retry_pending: 0,
            hash_retry_exhausted: 0,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gento_core::library;
    use gento_core::library::{HashedFile, ScanEntry, WalkOutcome};
    use gento_core::repo::{FileRepo, FileUpsert, ScanState};
    use std::sync::Mutex as StdMutex;

    /// イベント記録型テストホスト (AppHandle不要)。
    struct TestHost {
        progress_events: StdMutex<Vec<ScanProgressPayload>>,
        roots_changed: AtomicU64,
        snapshot_changed: AtomicU64,
    }

    impl TestHost {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                progress_events: StdMutex::new(Vec::new()),
                roots_changed: AtomicU64::new(0),
                snapshot_changed: AtomicU64::new(0),
            })
        }
    }

    impl LibraryHost for TestHost {
        fn scan_progress(&self, payload: ScanProgressPayload) {
            self.progress_events.lock().unwrap().push(payload);
        }
        fn roots_changed(&self) {
            self.roots_changed.fetch_add(1, Ordering::SeqCst);
        }
        fn library_snapshot_changed(&self) {
            self.snapshot_changed.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "gento-lib-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn write(&self, rel: &str, bytes: &[u8]) -> PathBuf {
            if let Some(parent) = Path::new(rel).parent() {
                std::fs::create_dir_all(self.0.join(parent)).unwrap();
            }
            let p = self.0.join(rel);
            std::fs::write(&p, bytes).unwrap();
            p
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp_db(name: &str) -> (PathBuf, TestDir) {
        let dir = TestDir::new(name);
        (dir.path().join("library.sqlite3"), dir)
    }

    fn make_controller(db_path: &Path) -> (Arc<LibraryController>, Arc<TestHost>) {
        let db = Arc::new(gento_db::Db::open(db_path).unwrap());
        let host = TestHost::new();
        let controller = LibraryController::new(db, host.clone()).unwrap();
        (controller, host)
    }

    /// 条件が成立するか期限までポーリングする。
    fn wait_until(secs: u64, mut predicate: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            if predicate() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        predicate()
    }

    fn count_state(controller: &LibraryController, root_id: i64, state: ScanState) -> u64 {
        use gento_core::repo::FileRepo;
        controller
            .db()
            .with_reader(|repos| {
                // 全件ページングで状態を数える (テスト規模は小さい)
                let mut offset = 0;
                let mut count = 0u64;
                loop {
                    let (page, total) = repos.list_files_paged(root_id, offset, 500)?;
                    for record in &page {
                        if record.scan_state == state {
                            count += 1;
                        }
                    }
                    offset += 500;
                    if offset as u64 >= total || page.is_empty() {
                        break;
                    }
                }
                Ok(count)
            })
            .unwrap_or(0)
    }

    const PNG_A: &[u8] = b"fake png A content for hashing";
    const PNG_B: &[u8] = b"fake png B different bytes";

    /// スキャン+ハッシュ完了まで待つ (hashed >= expect かつ pending==0)。
    fn wait_scan_settled(controller: &LibraryController, host: &TestHost, files: u64) -> bool {
        wait_until(20, || {
            let (found, pending) = {
                let progress = controller.shared.progress.lock().unwrap();
                let pending = controller.shared.hash_pending.load(Ordering::SeqCst);
                (progress.found, pending)
            };
            found >= files
                && pending == 0
                && !controller.is_scanning()
                && host.snapshot_changed.load(Ordering::SeqCst) > 0
        })
    }

    #[test]
    fn add_scan_hash_and_restart_restore() {
        // WP-2受入1/3: 登録→スキャン→ハッシュ→Asset統合→再起動復元。
        let (db_path, _dir) = temp_db("full");
        let data = TestDir::new("full-data");
        data.write("a.png", PNG_A);
        data.write("sub/b.png", PNG_A); // 同一内容
        data.write("c.png", PNG_B);

        let (controller, host) = make_controller(&db_path);
        let (root, overlapping) = controller.add_root(data.path()).unwrap();
        assert!(overlapping.is_empty());
        let _ = root;

        assert!(
            wait_scan_settled(&controller, &host, 3),
            "スキャン+ハッシュが完了する"
        );

        // 同一内容2ファイル → 1 Asset共有
        let (assets, linked) = controller
            .db()
            .with_reader(|repos| {
                let joined = repos.list_present_joined()?;
                let assets = joined.iter().filter(|(_, a)| a.is_some()).count();
                let mut ids: Vec<i64> = joined
                    .iter()
                    .filter_map(|(_, a)| a.as_ref().map(|a| a.id))
                    .collect();
                ids.sort();
                ids.dedup();
                Ok((assets, ids))
            })
            .unwrap();
        assert_eq!(linked.len(), 2, "Assetは2種 (A/B)");
        assert_eq!(assets, 3, "全ファイルがリンク済み");

        // スナップショット: 同一内容は同一AssetIdを共有
        let snapshot = controller.build_snapshot().unwrap();
        assert_eq!(snapshot.len(), 3);
        let id_of = |name: &str| snapshot.iter().find(|r| r.display_name == name).unwrap().id;
        assert_eq!(id_of("a.png"), id_of("b.png"), "同一内容 = 同一AssetId");
        assert_ne!(id_of("a.png"), id_of("c.png"));
        // 安定ID (assets.id。bit63なし)
        for record in &snapshot {
            assert_eq!(record.id.value() & library::PROVISIONAL_FILE_FLAG, 0);
        }

        // 再起動復元: 新しいコントローラ (≒プロセス再起動) で同一AssetId
        controller.shutdown();
        let (controller2, _host2) = make_controller(&db_path);
        let snapshot2 = controller2.build_snapshot().unwrap();
        let ids_before: Vec<_> = snapshot.iter().map(|r| r.id).collect();
        let ids_after: Vec<_> = snapshot2.iter().map(|r| r.id).collect();
        assert_eq!(ids_before, ids_after, "restart後も安定IDで復元");
        controller2.shutdown();
    }

    #[test]
    fn snapshot_id_encoding_matches_db_link_state() {
        // AssetId符号化の不変条件: asset_id NULLのFileは仮ID (bit63)、
        // リンク済みFileは assets.id と一致 (ハッシュ進捗に依存しない検証)。
        let (db_path, _dir) = temp_db("prov");
        let data = TestDir::new("prov-data");
        data.write("a.png", PNG_A);
        data.write("b.png", PNG_B);

        let (controller, host) = make_controller(&db_path);
        // ハッシュを即座に止めて、スキャンだけ先に完了させる
        let (root, _) = controller.add_root(data.path()).unwrap();
        controller.cancel_scan();
        assert!(wait_until(10, || {
            !controller.is_scanning() && controller.shared.progress.lock().unwrap().found >= 2
        }));

        let snapshot = controller.build_snapshot().unwrap();
        assert!(!snapshot.is_empty(), "走査のみ (hash未計算) でも一覧可能");
        let expected: Vec<(PathBuf, Option<i64>, i64)> = controller
            .db()
            .with_reader(|repos| {
                let joined = repos.list_present_joined()?;
                Ok(joined
                    .into_iter()
                    .map(|(f, a)| (f.rel_path.clone(), a.map(|a| a.id), f.id))
                    .collect())
            })
            .unwrap();
        for (rel, asset_id, file_id) in expected {
            let record = snapshot
                .iter()
                .find(|r| r.source_path.ends_with(&rel))
                .unwrap_or_else(|| panic!("{rel:?} がスナップショットに無い"));
            match asset_id {
                Some(expected_asset) => {
                    assert_eq!(
                        library::decode_library_asset_id(record.id),
                        library::LibraryAssetRef::Asset(expected_asset),
                        "リンク済みは assets.id (安定ID)"
                    );
                }
                None => {
                    assert_eq!(
                        library::decode_library_asset_id(record.id),
                        library::LibraryAssetRef::ProvisionalFile(file_id),
                        "未リンクは仮ID (bit63 | files.id)"
                    );
                }
            }
        }
        let _ = host;
        let _ = root;
        controller.shutdown();
    }

    #[test]
    fn add_root_validates_path_and_warns_overlap() {
        let (db_path, _dir) = temp_db("validate");
        let data = TestDir::new("validate-data");
        data.write("a.png", PNG_A);
        let (controller, _host) = make_controller(&db_path);

        // 未存在パス拒否
        let err = controller
            .add_root(Path::new("/nonexistent-gento-root"))
            .unwrap_err();
        assert!(err.contains("does not exist"), "{err}");

        // ファイル (ディレクトリでない) 拒否
        let file_path = data.path().join("a.png");
        let err = controller.add_root(&file_path).unwrap_err();
        assert!(err.contains("not a directory"), "{err}");

        let (root, _) = controller.add_root(data.path()).unwrap();
        let _ = &root;

        // 同一パス再登録拒否
        let err = controller.add_root(data.path()).unwrap_err();
        assert!(err.contains("already registered"), "{err}");

        // 親子重複: 配下サブフォルダを追加 → 警告付きで登録される
        let sub = data.write("sub/placeholder.png", PNG_A);
        let sub_dir = sub.parent().unwrap().to_path_buf();
        // canonicalizeされたパス同士の比較になるよう正規化
        let sub_canonical = std::fs::canonicalize(&sub_dir).unwrap();
        let (child, overlapping) = controller.add_root(&sub_canonical).unwrap();
        assert_eq!(overlapping.len(), 1, "親が検出される");
        assert!(overlapping[0].path != child.path);
        controller.shutdown();
    }

    #[test]
    fn watcher_converges_on_external_changes() {
        // WP-2受入2/5: 追加・変更・改名・削除が監視経由でDBへ反映され、
        // 最終状態が一致する。
        let (db_path, _dir) = temp_db("watch");
        let data = TestDir::new("watch-data");
        data.write("keep.png", PNG_A);
        data.write("modify.png", PNG_A);
        data.write("rename.png", PNG_B);
        data.write("delete.png", PNG_B);

        let (controller, host) = make_controller(&db_path);
        let (root, _) = controller.add_root(data.path()).unwrap();
        assert!(wait_scan_settled(&controller, &host, 4));

        // 外部変更: 追加・変更・改名 (inode保持)・削除
        data.write("new.png", PNG_B);
        let modified = b"modified content longer than before";
        std::fs::write(data.path().join("modify.png"), modified).unwrap();
        std::fs::rename(
            data.path().join("rename.png"),
            data.path().join("renamed.png"),
        )
        .unwrap();
        std::fs::remove_file(data.path().join("delete.png")).unwrap();

        // 収束待ち: present=4 (keep, modify, renamed, new) / missing=1 (delete)
        assert!(
            wait_until(20, || {
                count_state(&controller, root.id, ScanState::Present) == 4
                    && count_state(&controller, root.id, ScanState::Missing) == 1
            }),
            "外部変更が監視経由で反映される: present={} missing={}",
            count_state(&controller, root.id, ScanState::Present),
            count_state(&controller, root.id, ScanState::Missing)
        );

        // 改名は同一実体 (レコード移動。uuid不変) — パス検証
        let (_renamed_id, total_records) = controller
            .db()
            .with_reader(|repos| {
                let record = repos
                    .find_by_path(root.id, &PathBuf::from("renamed.png"))
                    .unwrap()
                    .expect("改名後パス");
                let (_, total) = repos.list_files_paged(root.id, 0, 1)?;
                Ok((record.id, total))
            })
            .unwrap();
        assert_eq!(total_records, 5, "deleteはmissing化で残る");
        assert!(controller
            .db()
            .with_reader(|repos| {
                Ok(repos
                    .find_by_path(root.id, &PathBuf::from("rename.png"))
                    .unwrap()
                    .is_none())
            })
            .unwrap());

        // 変更されたファイルの内容ハッシュが再計算され、別Assetへ切り替わる
        assert!(wait_until(20, || {
            controller
                .db()
                .with_reader(|repos| {
                    let record = repos
                        .find_by_path(root.id, &PathBuf::from("modify.png"))
                        .unwrap()
                        .unwrap();
                    Ok(record.byte_size == modified.len() as u64 && record.asset_id.is_some())
                })
                .unwrap_or(false)
        }));
        controller.shutdown();
    }

    #[test]
    fn watcher_burst_of_many_events_converges() {
        // WP-2受入5: 大量イベントバースト後も最終状態一致。
        let (db_path, _dir) = temp_db("burst");
        let data = TestDir::new("burst-data");
        data.write("seed.png", PNG_A);
        let (controller, host) = make_controller(&db_path);
        let (root, _) = controller.add_root(data.path()).unwrap();
        assert!(wait_scan_settled(&controller, &host, 1));

        // バースト: 60ファイルを一気に作成・一部を即削除
        for i in 0..60 {
            data.write(&format!("burst/f{i:02}.png"), PNG_B);
        }
        for i in 0..60 {
            if i % 3 == 0 {
                let _ = std::fs::remove_file(data.path().join(format!("burst/f{i:02}.png")));
            }
        }

        assert!(
            wait_until(25, || {
                let present = count_state(&controller, root.id, ScanState::Present);
                let missing = count_state(&controller, root.id, ScanState::Missing);
                present == 41 && missing == 0
            }),
            "バースト後の収束: present={} missing={}",
            count_state(&controller, root.id, ScanState::Present),
            count_state(&controller, root.id, ScanState::Missing)
        );
        controller.shutdown();
    }

    #[test]
    fn scan_stop_cancels_and_scan_start_resumes() {
        let (db_path, _dir) = temp_db("cancel");
        let data = TestDir::new("cancel-data");
        for i in 0..30 {
            data.write(&format!("c{i:02}.png"), PNG_A);
        }
        let (controller, host) = make_controller(&db_path);
        let (root, _) = controller.add_root(data.path()).unwrap();
        // すぐ停止
        controller.cancel_scan();
        assert!(wait_until(10, || !controller.is_scanning()));

        // 再開 (resume): 全ファイルが揃うまで再スキャン
        for _ in 0..5 {
            if wait_scan_settled(&controller, &host, 30) {
                break;
            }
            controller.enqueue_scan_all();
        }
        assert!(
            wait_until(10, || {
                count_state(&controller, root.id, ScanState::Present) == 30
            }),
            "再開で全ファイル収集"
        );
        controller.shutdown();
    }

    #[test]
    fn remove_root_deletes_files_but_keeps_assets() {
        let (db_path, _dir) = temp_db("remove");
        let data = TestDir::new("remove-data");
        data.write("a.png", PNG_A);
        let (controller, host) = make_controller(&db_path);
        let (root, _) = controller.add_root(data.path()).unwrap();
        assert!(wait_scan_settled(&controller, &host, 1));

        controller.remove_root(root.id).unwrap();

        let (roots, files, _assets) = controller
            .db()
            .with_reader(|repos| {
                let files = repos.list_files_paged(root.id, 0, 1).map(|(_, t)| t)?;
                Ok((repos.list_roots()?.len(), files, ()))
            })
            .unwrap();
        let asset_count = controller
            .db()
            .with_reader(|repos| {
                repos
                    .conn()
                    .query_row("SELECT count(*) FROM assets", [], |r| r.get::<_, i64>(0))
                    .map_err(|e| RepoError::Db(e.to_string()))
            })
            .unwrap();
        assert_eq!(roots, 0);
        assert_eq!(files, 0, "配下FileはCASCADE削除");
        assert_eq!(asset_count, 1, "Asset自体は残る (将来の再発見・統合用)");
        controller.shutdown();
    }

    #[test]
    fn disabled_root_is_skipped_by_scan() {
        let (db_path, _dir) = temp_db("disable");
        let data = TestDir::new("disable-data");
        data.write("a.png", PNG_A);
        let (controller, host) = make_controller(&db_path);
        let (root, _) = controller.add_root(data.path()).unwrap();
        assert!(wait_scan_settled(&controller, &host, 1));

        controller.set_root_enabled(root.id, false).unwrap();
        // 無効中にファイルを追加しても、スキャンでは拾わない
        data.write("ignored.png", PNG_B);
        controller.enqueue_scan_all();
        std::thread::sleep(Duration::from_millis(700));
        assert!(
            wait_until(10, || !controller.is_scanning()),
            "スキャン完了待ち"
        );
        let total = controller
            .db()
            .with_reader(|repos| repos.list_files_paged(root.id, 0, 1).map(|(_, t)| t))
            .unwrap();
        assert_eq!(total, 1, "無効ルートはスキャン対象外: {total}");

        // 有効化で再び拾う
        controller.set_root_enabled(root.id, true).unwrap();
        assert!(wait_until(15, || {
            controller
                .db()
                .with_reader(|repos| repos.list_files_paged(root.id, 0, 1).map(|(_, t)| t))
                .map(|t| t == 2)
                .unwrap_or(false)
        }));
        controller.shutdown();
    }

    // ===== FG-WP2: overflow収束・ハッシュ会計・watcher寿命・劣化運用 =====

    /// テスト用Shared直接構築 (debouncer・会計の単体試験)。
    fn make_shared(db_path: &Path) -> Arc<Shared> {
        let db = Arc::new(gento_db::Db::open(db_path).unwrap());
        Arc::new(Shared {
            db,
            fs: Arc::new(PlatformFs),
            host: TestHost::new(),
            cancel: AtomicBool::new(false),
            progress: Mutex::new(LibraryProgress::default()),
            hash_pending: AtomicU64::new(0),
            last_hash_emit: Mutex::new(None),
            scanning: AtomicBool::new(false),
            hash_retry: Mutex::new(HashRetry::default()),
            hash_queue_capacity: 8_192,
        })
    }

    #[test]
    fn overflow_without_normal_events_schedules_full_rescan() {
        // overflow (watcherエラー・未知イベント種) 単独でも、通常イベントが
        // 来なくても全体再スキャンが発火する (Blocker 4)。
        let (db_path, _dir) = temp_db("overflow-only");
        let shared = make_shared(&db_path);
        let buffer = Arc::new(WatchBuffer::new());
        let (event_tx, event_rx) = mpsc::channel::<(FsChange, PathBuf)>();
        let stop = Arc::new(AtomicBool::new(false));
        let (cmd_tx, cmd_rx) = mpsc::channel::<LibCmd>();
        let watchers: Watchers = Arc::new(Mutex::new(HashMap::new()));
        let handle = spawn_debouncer(DebouncerParams {
            buffer: Arc::clone(&buffer),
            event_rx,
            stop,
            root: PathBuf::from("/data/photos"),
            root_id: 1,
            shared: Arc::clone(&shared),
            cmd_tx,
            watchers,
        });
        // 通常イベントを1件も送らずoverflowのみ
        buffer.mark_overflow();
        match cmd_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(LibCmd::ScanRoot(root_id)) => assert_eq!(root_id, 1),
            _other => panic!("overflow単独でScanRootが来るべき"),
        }
        drop(event_tx); // 切断でdebouncer終了
        let _ = handle.join();
        assert!(!buffer.is_overflow(), "drainでフラグ解除済み");
    }

    #[test]
    fn hash_accounting_is_exact_under_dedup_queue_saturation_and_shutdown() {
        // Blocker 5: pending_hash = submit成功数 − 完了通知数 が厳密に成立する。
        // (a) dedup統合は二重計上しない (b) queue満杯の放棄分は数えない
        // (c) pool shutdownの放棄分も完了通知 (Cancelled) で相殺されて0になる。
        let (db_path, _dir) = temp_db("accounting");
        let shared = make_shared(&db_path);
        let pending_of = |id: i64| PendingHash {
            file_id: id,
            path: PathBuf::from("/nonexistent/does-not-matter.png"),
            token: GenerationToken::default(),
        };

        // (a) dedup: 同一file_idの再提出はMergedExisting → pending増えない
        let (pool_a, rx_a) = WorkerPool::new(PoolConfig {
            worker_count: 1,
            queue_capacity: 8,
        });
        pool_a.set_paused(true);
        let accountant_a =
            spawn_hash_accountant(Arc::clone(&shared), rx_a, Arc::downgrade(&pool_a));
        enqueue_hash_jobs(&shared, &pool_a, vec![pending_of(1)]);
        assert_eq!(shared.hash_pending.load(Ordering::SeqCst), 1);
        enqueue_hash_jobs(&shared, &pool_a, vec![pending_of(1)]);
        assert_eq!(
            shared.hash_pending.load(Ordering::SeqCst),
            1,
            "dedup統合で増えない"
        );
        pool_a.set_paused(false);
        assert!(wait_until(10, || shared
            .hash_pending
            .load(Ordering::SeqCst)
            == 0));
        // workerがPoolInnerを保持するため、shutdownでworkerを止めないと
        // 完了チャネルが閉じず会計スレッドが終了しない。
        pool_a.shutdown();
        drop(pool_a);
        let _ = accountant_a.join();

        // (b) queue満杯: capacity=2 に5件 → 2件のみ計上
        // (FG-WP2-2: 拒否された3件は決定論リトライキューへ移される)
        shared.hash_pending.store(0, Ordering::SeqCst);
        let (pool_b, rx_b) = WorkerPool::new(PoolConfig {
            worker_count: 1,
            queue_capacity: 2,
        });
        pool_b.set_paused(true);
        let accountant_b =
            spawn_hash_accountant(Arc::clone(&shared), rx_b, Arc::downgrade(&pool_b));
        enqueue_hash_jobs(
            &shared,
            &pool_b,
            (1..=5).map(pending_of).collect::<Vec<_>>(),
        );
        assert_eq!(
            shared.hash_pending.load(Ordering::SeqCst),
            2,
            "満杯で拒否された3件は計上しない"
        );
        assert_eq!(
            shared.hash_retry.lock().unwrap().queue.len(),
            3,
            "拒否分はリトライキューへ"
        );

        // (c) shutdown放棄: キュー残りはCancelled完了通知で相殺 → 厳密に0
        pool_b.shutdown();
        drop(pool_b);
        let _ = accountant_b.join();
        assert_eq!(
            shared.hash_pending.load(Ordering::SeqCst),
            0,
            "終了时会計は厳密に0"
        );
    }

    #[test]
    fn cancelled_hash_jobs_settle_pending_to_zero() {
        // キャンセル (ジョブ実行前・ハッシュ中の両形態) でも会計は0に収束。
        let (db_path, _dir) = temp_db("cancel-accounting");
        let data = TestDir::new("cancel-acc-data");
        for i in 0..30 {
            data.write(&format!("c{i:02}.png"), PNG_A);
        }
        let (controller, host) = make_controller(&db_path);
        let (root, _) = controller.add_root(data.path()).unwrap();
        // すぐキャンセル → 実行前キャンセルとハッシュ中キャンセルが混在
        controller.cancel_scan();
        assert!(wait_until(10, || !controller.is_scanning()));

        for _ in 0..8 {
            if wait_scan_settled(&controller, &host, 30) {
                break;
            }
            controller.enqueue_scan_all();
        }
        assert!(
            wait_until(15, || {
                count_state(&controller, root.id, ScanState::Present) == 30
                    && controller.shared.hash_pending.load(Ordering::SeqCst) == 0
            }),
            "再開で全ファイル収集・会計0: present={} pending={}",
            count_state(&controller, root.id, ScanState::Present),
            controller.shared.hash_pending.load(Ordering::SeqCst)
        );
        controller.shutdown();
        assert_eq!(controller.shared.hash_pending.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn maintenance_degraded_polling_rescans_when_watcher_unavailable() {
        // watcher確立失敗 (inotify上限等の模擬) でもメンテナンス周期で
        // ポーリング再スキャンが投入され、放っておかれない (非ブロッカー10)。
        let (db_path, _dir) = temp_db("degraded");
        let data = TestDir::new("degraded-data");
        data.write("a.png", PNG_A);
        let db = Arc::new(gento_db::Db::open(&db_path).unwrap());
        let root = db.with_writer(|repos| repos.add_root(data.path())).unwrap();
        let shared = make_shared(&db_path);
        let _ = db;
        let (tx, rx) = mpsc::channel::<LibCmd>();
        let watchers: Watchers = Arc::new(Mutex::new(HashMap::new()));

        run_maintenance(&shared, &watchers, &tx, &|_| {
            Err("inotify watch limit reached (simulated)".to_string())
        });
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(LibCmd::ScanRoot(root_id)) => assert_eq!(root_id, root.id),
            _other => panic!("劣化時もポーリング再スキャン"),
        }
        assert!(watchers.lock().unwrap().is_empty(), "確立失敗なら登録なし");

        // 復帰後 (確立成功) も再スキャン + watcher登録
        run_maintenance(&shared, &watchers, &tx, &|_| Ok(()));
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(LibCmd::ScanRoot(root_id)) => assert_eq!(root_id, root.id),
            _other => panic!("復帰時も再スキャン"),
        }
    }

    #[test]
    fn watch_degraded_reflected_in_scan_status() {
        let (db_path, _dir) = temp_db("status-degraded");
        let data = TestDir::new("status-degraded-data");
        data.write("a.png", PNG_A);
        let (controller, _host) = make_controller(&db_path);
        let (root, _) = controller.add_root(data.path()).unwrap();

        assert!(
            wait_until(10, || controller
                .watchers
                .lock()
                .unwrap()
                .contains_key(&root.id)),
            "watcher確立待ち"
        );
        assert!(!controller.status_dto().watch_degraded);

        // watcher喪失 (上限到達等) を模擬 → degraded
        stop_watcher(&controller.watchers, root.id);
        let status = controller.status_dto();
        assert!(status.watch_degraded, "未確立はstatusへ露出");

        // watch_enabled=false のルートは degraded扱いしない
        controller
            .shared
            .db
            .with_writer(|repos| {
                repos.set_root_settings(
                    root.id,
                    RootSettings {
                        watch_enabled: false,
                        ..RootSettings::default()
                    },
                )
            })
            .unwrap();
        assert!(!controller.status_dto().watch_degraded);
        controller.shutdown();
    }

    #[test]
    fn maintenance_reestablishes_watcher_after_root_reappears() {
        // Blocker 9: ルート不在化でwatcher解放 → 再出現で再確立・再スキャン。
        let (db_path, _dir) = temp_db("reappear");
        let data = TestDir::new("reappear-data");
        data.write("a.png", PNG_A);
        let db = Arc::new(gento_db::Db::open(&db_path).unwrap());
        let host = TestHost::new();
        let fs: Arc<dyn ScanFs> = Arc::new(PlatformFs);
        let controller = LibraryController::with_fs_maintenance(
            db,
            host.clone(),
            fs,
            Duration::from_millis(200),
            8_192,
        )
        .unwrap();
        let (root, _) = controller.add_root(data.path()).unwrap();
        assert!(wait_scan_settled(&controller, &host, 1));

        // ルートごと削除 (アンマウント相当)
        std::fs::remove_file(data.path().join("a.png")).unwrap();
        std::fs::remove_dir(data.path()).unwrap();
        assert!(
            wait_until(15, || !controller
                .watchers
                .lock()
                .unwrap()
                .contains_key(&root.id)),
            "debouncer自己解放 or メンテナンス解放でwatcherが外れる"
        );

        // 再出現 → メンテナンスがwatcher再確立 + 再スキャン
        std::fs::create_dir_all(data.path()).unwrap();
        std::fs::write(data.path().join("a.png"), PNG_A).unwrap();
        assert!(
            wait_until(15, || {
                controller.watchers.lock().unwrap().contains_key(&root.id)
                    && count_state(&controller, root.id, ScanState::Present) == 1
            }),
            "再出現でwatcher再確立・present復帰: present={}",
            count_state(&controller, root.id, ScanState::Present)
        );
        controller.shutdown();
    }

    #[cfg(unix)]
    #[test]
    fn permission_denied_subtree_is_not_missing_marked() {
        if std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("Uid:"))
                    .and_then(|l| l.split_whitespace().nth(1).map(|u| u == "0"))
            })
            .unwrap_or(false)
        {
            return; // rootは chmod 000 でも読めてしまうため検証にならない
        }
        use std::os::unix::fs::PermissionsExt;
        let (db_path, _dir) = temp_db("perm-subtree");
        let data = TestDir::new("perm-subtree-data");
        data.write("keep.png", PNG_A);
        data.write("locked/x.png", PNG_B);
        let (controller, host) = make_controller(&db_path);
        let (root, _) = controller.add_root(data.path()).unwrap();
        assert!(wait_scan_settled(&controller, &host, 2));

        // locked/ を読み取り不能にして再スキャン → 配下はmissing化しない
        std::fs::set_permissions(
            data.path().join("locked"),
            std::fs::Permissions::from_mode(0o000),
        )
        .unwrap();
        controller.enqueue_scan_all();
        assert!(
            wait_until(15, || {
                let status = controller.status_dto();
                !status.scanning && status.errors >= 1
            }),
            "不完全走査はエラーとして状態に反映される"
        );
        let (present, missing) = (
            count_state(&controller, root.id, ScanState::Present),
            count_state(&controller, root.id, ScanState::Missing),
        );
        assert_eq!(
            present, 2,
            "locked配下もmissing化しない (present={present} missing={missing})"
        );
        assert_eq!(missing, 0);

        // 権限復帰 → 通常運用に戻る
        std::fs::set_permissions(
            data.path().join("locked"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        controller.enqueue_scan_all();
        assert!(wait_until(15, || {
            let status = controller.status_dto();
            !status.scanning && status.errors == 0
        }));
        assert_eq!(count_state(&controller, root.id, ScanState::Present), 2);
        controller.shutdown();
    }

    #[test]
    fn remove_root_while_scanning_and_hashing_keeps_controller_healthy() {
        // スキャン・ハッシュ稼働中のルート削除: パニックせず、会計も収束し、
        // controllerは引き続き機能する。
        let (db_path, _dir) = temp_db("remove-mid");
        let data = TestDir::new("remove-mid-data");
        for i in 0..200 {
            data.write(&format!("f{i:03}.png"), PNG_A);
        }
        let (controller, _host) = make_controller(&db_path);
        let (root, _) = controller.add_root(data.path()).unwrap();
        // スキャン・ハッシュが動いているうちに削除
        controller.remove_root(root.id).unwrap();

        assert!(wait_until(20, || {
            !controller.is_scanning() && controller.shared.hash_pending.load(Ordering::SeqCst) == 0
        }));
        let roots = controller
            .db()
            .with_reader(|repos| repos.list_roots())
            .unwrap()
            .len();
        assert_eq!(roots, 0, "ルートは削除済み");

        // controllerは新しいルートで引き続き機能する
        let data2 = TestDir::new("remove-mid-data2");
        data2.write("after.png", PNG_B);
        let (root2, _) = controller.add_root(data2.path()).unwrap();
        assert!(wait_until(20, || {
            count_state(&controller, root2.id, ScanState::Present) == 1
                && controller.shared.hash_pending.load(Ordering::SeqCst) == 0
                && !controller.is_scanning()
        }));
        controller.shutdown();
        assert_eq!(
            controller.shared.hash_pending.load(Ordering::SeqCst),
            0,
            "終了时会計0"
        );
    }

    // ===== FG-WP2-2: 破棄/拒否ジョブの決定論リトライ =====

    /// hash_and_probe差し替えフック: (path, 呼び出し回数) → 差し替え結果。
    type HashRig =
        Box<dyn Fn(&Path, u64) -> Option<Result<HashedFile, HashFileError>> + Send + Sync>;

    /// PlatformFsへ委譲しつつ hash_and_probe を差し替え可能なfs
    /// (リトライ試験用)。`(path, これまでの呼び出し回数)` → Some(result) で
    /// 差し替え、Noneなら実処理へ委譲。遅延注入も可能。
    struct RiggedHashFs {
        rig: HashRig,
        delay_ms: u64,
        hash_calls: StdMutex<HashMap<PathBuf, u64>>,
        walk_calls: AtomicU64,
    }

    impl RiggedHashFs {
        fn new(rig: HashRig) -> Self {
            Self {
                rig,
                delay_ms: 0,
                hash_calls: StdMutex::new(HashMap::new()),
                walk_calls: AtomicU64::new(0),
            }
        }

        fn with_delay(mut self, delay_ms: u64) -> Self {
            self.delay_ms = delay_ms;
            self
        }

        fn hash_calls_of(&self, path: &Path) -> u64 {
            *self.hash_calls.lock().unwrap().get(path).unwrap_or(&0)
        }
    }

    impl ScanFs for RiggedHashFs {
        fn walk(
            &self,
            root: &Path,
            scope: Option<&Path>,
            sink: &mut dyn FnMut(ScanEntry),
        ) -> WalkOutcome {
            self.walk_calls.fetch_add(1, Ordering::SeqCst);
            PlatformFs.walk(root, scope, sink)
        }
        fn hash_and_probe(
            &self,
            path: &Path,
            expect: &GenerationToken,
            cancel: &AtomicBool,
        ) -> Result<HashedFile, HashFileError> {
            let calls = {
                let mut map = self.hash_calls.lock().unwrap();
                let entry = map.entry(path.to_path_buf()).or_insert(0);
                *entry += 1;
                *entry
            };
            if let Some(result) = (self.rig)(path, calls) {
                return result;
            }
            if self.delay_ms > 0 {
                std::thread::sleep(Duration::from_millis(self.delay_ms));
            }
            PlatformFs.hash_and_probe(path, expect, cancel)
        }
    }

    /// リンク済み (asset_id非NULL) のpresentファイル数 (全ルート)。
    fn linked_count(controller: &LibraryController) -> u64 {
        controller
            .db()
            .with_reader(|repos| {
                let joined = repos.list_present_joined()?;
                Ok(joined.iter().filter(|(_, a)| a.is_some()).count() as u64)
            })
            .unwrap_or(0)
    }

    #[test]
    fn queue_full_jobs_complete_via_completion_driven_retry() {
        // FG-WP2-2: queue満杯で拒否されたジョブは、監視イベント・再スキャンが
        // なくてもプール完了駆動の再提出で最終的にハッシュ完了する。
        let (db_path, _dir) = temp_db("retry-queuefull");
        let data = TestDir::new("retry-queuefull-data");
        for i in 0..20 {
            data.write(&format!("f{i:02}.png"), PNG_A);
        }
        // 遅延ハッシュで走査のsubmitループ中にキューを確実に満杯にする
        let fs: Arc<dyn ScanFs> =
            Arc::new(RiggedHashFs::new(Box::new(|_path, _calls| None)).with_delay(30));
        let db = Arc::new(gento_db::Db::open(&db_path).unwrap());
        let host = TestHost::new();
        let controller = LibraryController::with_fs_maintenance(
            db,
            host.clone(),
            fs,
            Duration::from_secs(3600), // メンテナンス経路を排し完了駆動のみで証明
            4,                         // 小容量キューで確実に満杯にする
        )
        .unwrap();
        let (root, _) = controller.add_root(data.path()).unwrap();

        // 待機中にretry経路を通ったことを観測 (status露出値のポーリング)
        let mut saw_retry_pending = false;
        let settled = wait_until(30, || {
            let status = controller.status_dto();
            saw_retry_pending |= status.hash_retry_pending > 0;
            status.hash_retry_pending == 0
                && status.pending_hash == 0
                && !status.scanning
                && linked_count(&controller) == 20
        });
        assert!(
            settled,
            "queue満杯 → 完了駆動再提出で全ファイル解決: status={:?}",
            controller.status_dto()
        );
        assert!(
            saw_retry_pending,
            "テスト中にqueue満杯の拒否が実際に起きたこと"
        );
        assert_eq!(count_state(&controller, root.id, ScanState::Present), 20);
        controller.shutdown();
    }

    #[test]
    fn stale_discarded_job_is_resubmitted_without_rescan() {
        // FG-WP2-2: Stale破棄されたジョブは再スキャンなしで決定論的に再提出
        // される (完了駆動)。2回目の試行で実ハッシュ → リンク。
        let (db_path, _dir) = temp_db("retry-stale");
        let data = TestDir::new("retry-stale-data");
        let target = data.write("a.png", PNG_A);
        let rigged_path = target.clone();
        let fs = Arc::new(RiggedHashFs::new(Box::new(move |path, calls| {
            if path == rigged_path && calls == 1 {
                Some(Err(HashFileError::Stale(
                    "injected first-attempt stale".into(),
                )))
            } else {
                None
            }
        })));
        let db = Arc::new(gento_db::Db::open(&db_path).unwrap());
        let host = TestHost::new();
        let controller = LibraryController::with_fs_maintenance(
            db,
            host.clone(),
            fs.clone(),
            Duration::from_secs(3600), // メンテナンス経路を排し完了駆動のみで証明
            8_192,
        )
        .unwrap();
        let (root, _) = controller.add_root(data.path()).unwrap();

        assert!(
            wait_until(20, || {
                count_state(&controller, root.id, ScanState::Present) == 1
                    && linked_count(&controller) == 1
            }),
            "Stale後の再提出でリンクされる: status={:?}",
            controller.status_dto()
        );
        assert!(
            fs.hash_calls_of(&target) >= 2,
            "同一ファイルが再提出されている (calls={})",
            fs.hash_calls_of(&target)
        );
        assert_eq!(
            fs.walk_calls.load(Ordering::SeqCst),
            1,
            "再スキャンなしで解決 (walkは初回スキャンの1回のみ)"
        );
        assert_eq!(controller.status_dto().hash_retry_pending, 0);
        controller.shutdown();
    }

    #[test]
    fn stale_retry_attempt_cap_is_exposed_and_scan_rearms() {
        // FG-WP2-2: Stale試行上限到達は断念してstatusへ露出。次のスキャン
        // (新世代トークンのキューイング) で再試行状態はリセットされる。
        let (db_path, _dir) = temp_db("retry-cap");
        let data = TestDir::new("retry-cap-data");
        let target = data.write("a.png", PNG_A);
        let always_stale = Arc::new(AtomicBool::new(true));
        let flag = Arc::clone(&always_stale);
        let rigged_path = target.clone();
        let fs = Arc::new(RiggedHashFs::new(Box::new(move |path, _calls| {
            if path == rigged_path && flag.load(Ordering::SeqCst) {
                Some(Err(HashFileError::Stale(
                    "injected persistent stale".into(),
                )))
            } else {
                None
            }
        })));
        let db = Arc::new(gento_db::Db::open(&db_path).unwrap());
        let host = TestHost::new();
        let controller = LibraryController::with_fs_maintenance(
            db,
            host.clone(),
            fs.clone(),
            Duration::from_secs(3600),
            8_192,
        )
        .unwrap();
        let (root, _) = controller.add_root(data.path()).unwrap();
        let _ = root;

        // 上限到達 → 断念がstatusに露出し、再試行も止まる
        assert!(
            wait_until(20, || {
                let status = controller.status_dto();
                status.hash_retry_exhausted >= 1 && status.hash_retry_pending == 0
            }),
            "試行上限到達はstatusへ露出: status={:?}",
            controller.status_dto()
        );
        let attempts_at_cap = fs.hash_calls_of(&target);
        assert!(attempts_at_cap >= HASH_RETRY_MAX_ATTEMPTS as u64);

        // 断念後は再提出されない (呼び出し回数が伸びない)
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(fs.hash_calls_of(&target), attempts_at_cap);

        // 注入を解除して再スキャン → リセットされてリンクされる
        always_stale.store(false, Ordering::SeqCst);
        controller.enqueue_scan_all();
        assert!(
            wait_until(20, || linked_count(&controller) == 1),
            "再スキャンで再試行状態がリセットされ解決: status={:?}",
            controller.status_dto()
        );
        let status = controller.status_dto();
        assert_eq!(status.hash_retry_exhausted, 0, "スキャンで解除");
        controller.shutdown();
    }

    #[test]
    fn maintenance_resolves_immutable_unlinked_file_without_watch_events() {
        // FG-WP2-2: 監視健全な通常ルートでも「不変な未リンクファイル」が
        // 永続未解決にならない。スキャン・監視イベントなしで、メンテナンス
        // 周期の未リンクpresentファイル再提出だけで解決する。
        let (db_path, _dir) = temp_db("retry-maintenance");
        let data = TestDir::new("maintenance-data");
        let file = data.write("a.png", PNG_A);

        // DBへルート + 未リンクfile行を事前シード (実ファイルと同一トークン)
        let db = Arc::new(gento_db::Db::open(&db_path).unwrap());
        let root = db.with_writer(|repos| repos.add_root(data.path())).unwrap();
        let token = gento_image::hash::stat_token(&file).expect("stat token");
        db.with_writer(|repos| {
            repos.upsert_file(FileUpsert {
                root_id: root.id,
                rel_path: PathBuf::from("a.png"),
                display_name: "a.png".to_string(),
                byte_size: token.byte_size,
                mtime_ms: token.mtime_ns / 1_000_000,
                mtime_ns: token.mtime_ns,
                scan_state: ScanState::Present,
                os_file_id: token.os_file_id,
                detach_asset: false,
            })
        })
        .unwrap();

        let fs = Arc::new(RiggedHashFs::new(Box::new(|_path, _calls| None)));
        let host = TestHost::new();
        // 短いメンテナンス周期 (200ms)。add_rootしない = 初回スキャンなし。
        // 監視は健全に確立しておく (イベントは発生しない不変ファイル)。
        let controller = LibraryController::with_fs_maintenance(
            db,
            host.clone(),
            fs.clone(),
            Duration::from_millis(200),
            8_192,
        )
        .unwrap();
        let (root_record, available) = controller
            .list_roots()
            .unwrap()
            .into_iter()
            .find(|(record, _)| record.id == root.id)
            .expect("seeded root");
        assert!(available);
        controller.start_root_watcher(&root_record).unwrap();
        assert!(
            wait_until(20, || linked_count(&controller) == 1),
            "メンテナンスの再提出だけで未リンクファイルが解決: status={:?}",
            controller.status_dto()
        );
        assert_eq!(
            fs.walk_calls.load(Ordering::SeqCst),
            0,
            "スキャンなしで解決 (walk呼び出しゼロ)"
        );
        controller.shutdown();
    }
}
