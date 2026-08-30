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
//! - イベント欠落 (overflow等) は全体再スキャンで収束。
//! - ルート削除・アンマウント: watcherを停止し `roots:changed` を通知。
//!   DBのルート行とFile行は保持 (「不在ルート=無効表示、削除しない」)。
//!
//! # スキャンの逐次化
//!
//! 全コマンド (全体スキャン・スコープ差分) は単一ワーカースレッドの
//! キューへ投入され順次実行される。初回スキャン中に到着した監視イベントは
//! キューへ蓄積され、スキャン完了後に適用される (バッファリング)。

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gento_core::library::{
    self, hash_and_link_file, scan_root, FsChange, ScanFs, ScanOptions, ScanPhase, ScanProgress,
    WatchBuffer,
};
use gento_core::repo::{FileRepo, LibraryRootRecord, LibraryRootRepo, RepoError, RootSettings};
use gento_core::Priority;
use gento_image::fs::PlatformFs;
use gento_image::pool::{Job, PoolConfig, SubmitError, WorkerPool};
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

/// 登録ルート制御中枢。
pub struct LibraryController {
    shared: Arc<Shared>,
    tx: Sender<LibCmd>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
    watchers: Mutex<HashMap<i64, WatchState>>,
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
        let (tx, rx) = mpsc::channel::<LibCmd>();
        let hash_pool = WorkerPool::new(PoolConfig {
            worker_count: 2,
            queue_capacity: 8_192,
        })
        .0;
        let shared = Arc::new(Shared {
            db,
            fs,
            host,
            cancel: AtomicBool::new(false),
            progress: Mutex::new(LibraryProgress::default()),
            hash_pending: AtomicU64::new(0),
            last_hash_emit: Mutex::new(None),
            scanning: AtomicBool::new(false),
        });
        let worker_shared = Arc::clone(&shared);
        let worker_pool = Arc::clone(&hash_pool);
        let worker = std::thread::Builder::new()
            .name("library-worker".into())
            .spawn(move || worker_loop(worker_shared, worker_pool, rx))
            .map_err(|e| RepoError::Db(format!("spawn library worker: {e}")))?;
        Ok(Arc::new(Self {
            shared,
            tx,
            worker: Mutex::new(Some(worker)),
            watchers: Mutex::new(HashMap::new()),
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
        if let Err(err) = self.start_watcher(&root) {
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
                    let _ = self.start_watcher(root);
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
        self.start_watcher(root)
    }

    /// ルートの監視を開始する。
    fn start_watcher(&self, root: &LibraryRootRecord) -> Result<(), String> {
        let mut watchers = self.watchers.lock().unwrap_or_else(|p| p.into_inner());
        if watchers.contains_key(&root.id) {
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
        let debouncer = spawn_debouncer(
            Arc::clone(&buffer),
            event_rx,
            Arc::clone(&stop),
            root_path,
            root.id,
            Arc::clone(&self.shared),
            self.tx.clone(),
        );
        watchers.insert(
            root.id,
            WatchState {
                _watcher: watcher,
                debouncer: Some(debouncer),
                stop,
            },
        );
        Ok(())
    }

    /// ルートの監視を停止する。
    fn stop_watcher(&self, root_id: i64) {
        let mut watchers = self.watchers.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(mut state) = watchers.remove(&root_id) {
            state.stop.store(true, Ordering::SeqCst);
            if let Some(handle) = state.debouncer.take() {
                let _ = handle.join();
            }
            // watcherはここでdropされinotify監視が解除される
        }
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
    }

    fn status_dto(&self) -> LibraryScanStatusDto {
        let progress = *self
            .shared
            .progress
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let pending = self.shared.hash_pending.load(Ordering::SeqCst);
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
        }
    }
}

/// debounceスレッド: ウィンドウ内のイベントを結合しスコープ差分へ変換する。
fn spawn_debouncer(
    buffer: Arc<WatchBuffer>,
    event_rx: Receiver<(FsChange, PathBuf)>,
    stop: Arc<AtomicBool>,
    root: PathBuf,
    root_id: i64,
    shared: Arc<Shared>,
    cmd_tx: Sender<LibCmd>,
) -> std::thread::JoinHandle<()> {
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
                let Some(last) = last_event else { continue };
                if last.elapsed() < debounce && !buffer.is_overflow() {
                    continue;
                }
                last_event = None;
                let coalesced = buffer.drain();
                if coalesced.changes.is_empty() && !coalesced.overflow {
                    continue;
                }
                // ルート自体の削除 (アンマウント含む): 監視停止 + 通知のみ。
                if coalesced
                    .changes
                    .get(&root)
                    .is_some_and(|kind| *kind == FsChange::Removed)
                {
                    shared.host.roots_changed();
                    tracing::info!(root_id, "root removed; stopping watcher");
                    // 自スレッドの停止フラグはstop_watcherが立てる
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
/// スコープコマンドを次周でマージする。
fn worker_loop(shared: Arc<Shared>, hash_pool: Arc<WorkerPool>, rx: Receiver<LibCmd>) {
    while let Ok(cmd) = rx.recv() {
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

/// ハッシュジョブ (Background優先度)。完了でhashed進捗を進める。
struct HashJob {
    shared: Arc<Shared>,
    file_id: i64,
    path: PathBuf,
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
            &self.shared.cancel,
        ) {
            Ok(_asset) => {
                self.shared.hash_pending.fetch_sub(1, Ordering::SeqCst);
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
            Err(gento_core::library::HashFileError::Cancelled) => Err("cancelled".to_string()),
            Err(err) => {
                self.shared.hash_pending.fetch_sub(1, Ordering::SeqCst);
                update_progress(&self.shared, |current| current.errors += 1);
                Err(err.to_string())
            }
        }
    }
}

fn enqueue_hash_jobs(
    shared: &Arc<Shared>,
    hash_pool: &Arc<WorkerPool>,
    pending: Vec<library::PendingHash>,
) {
    if pending.is_empty() {
        return;
    }
    let total = shared
        .hash_pending
        .fetch_add(pending.len() as u64, Ordering::SeqCst)
        + pending.len() as u64;
    tracing::debug!(
        count = pending.len(),
        total_pending = total,
        "hash jobs enqueued"
    );
    for item in pending {
        let job = Arc::new(HashJob {
            shared: Arc::clone(shared),
            file_id: item.file_id,
            path: item.path,
        });
        let key = format!("hash:{}", item.file_id);
        if let Err(err) = hash_pool.submit(&key, Priority::Background, job) {
            shared.hash_pending.fetch_sub(1, Ordering::SeqCst);
            match err {
                SubmitError::QueueFull => {
                    // バックプレッシャー: 残りは次回スキャンのresumeに任せる
                    tracing::warn!("hash queue full; remaining files will be retried on next scan");
                    break;
                }
                SubmitError::Shutdown => break,
            }
        }
    }
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
        let (id, width, height, rating, triage) = match &asset {
            Some(asset) => (
                library_asset_id(asset.id),
                asset.width,
                asset.height,
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
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gento_core::repo::{FileRepo, ScanState};
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
}
