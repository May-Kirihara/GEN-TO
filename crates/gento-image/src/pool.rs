//! 上限付き・優先度付きワーカープール (協調キャンセル・重複統合を含む)
//! (Phase 4 / WP-3)。
//!
//! 設計契約 (PM計画 §3、WP-3委任仕様):
//! - 固定ワーカー数 = min(4, available_parallelism)
//!   (WP-16: 環境変数 `GENTO_THUMB_WORKERS` で上書き可。計測用ノブ)
//! - 優先度: gento_core::Priority (値が小さいほど優先)。同一優先度内は単調seqでFIFO
//! - 再優先度化: キー指定で未開始ジョブの優先度更新
//!   (停滞ヒープエントリはlive_entry照合でpop時に無効化)
//! - キャンセル: 未開始=即時除去 (lazy無効化)。
//!   実行中=協調フラグ (CancelToken) をジョブが任意の時点で検査
//! - dedup: 同一キーの同時要求は1ジョブに統合し、完了時に全待機者へ通知
//! - 上限付き入場: キュー上限 (既定4096)。満杯時はErrで拒否
//! - panic隔離: catch_unwindで包み、失敗扱い+ワーカー継続
//! - 一時停止/再開: AtomicBool + Condvar (実行中ジョブは継続)
//! - シャットダウン: 停止フラグ→待機中ジョブをCancelled完了化→ワーカーjoin
//!
//! ジョブは [`Job`] traitで受け取り、単体テストはフェイクジョブで高速化する。

use std::cmp::Ordering as CmpOrdering;
use std::collections::{BinaryHeap, HashMap};
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use gento_core::Priority;

/// 協調キャンセルトークン。ジョブはデコード前/リサイズ前/エンコード前等の
/// 区切りで検査する。
#[derive(Debug, Clone)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

/// ワーカーで実行するジョブ。`run` はワーカースレッドで呼ばれる。
pub trait Job: Send + Sync {
    fn run(&self, cancel: &CancelToken) -> Result<Vec<u8>, String>;
}

/// ジョブの完了結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Completed(Vec<u8>),
    Failed(String),
    Cancelled,
}

impl Outcome {
    /// バイト列を除いた要約 (完了チャネル・イベント送出用)。
    pub fn summary(&self) -> OutcomeSummary {
        match self {
            Outcome::Completed(_) => OutcomeSummary::Completed,
            Outcome::Failed(reason) => OutcomeSummary::Failed(reason.clone()),
            Outcome::Cancelled => OutcomeSummary::Cancelled,
        }
    }
}

/// 完了チャネルに流れる要約。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutcomeSummary {
    Completed,
    Failed(String),
    Cancelled,
}

/// 完了通知1件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobCompletion {
    pub key: String,
    pub outcome: OutcomeSummary,
}

/// submitの結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitOutcome {
    /// 新規ジョブとして投入された。
    Enqueued(JobHandle),
    /// 同一キーのジョブが既に存在し、その完了待ちに統合された。
    MergedExisting(JobHandle),
}

impl SubmitOutcome {
    /// どちらのvariantでも完了待ちハンドルを取り出す。
    pub fn handle(self) -> JobHandle {
        match self {
            SubmitOutcome::Enqueued(h) | SubmitOutcome::MergedExisting(h) => h,
        }
    }

    /// 統合 (既存ジョブへ合流) だったか。
    pub fn is_merged(&self) -> bool {
        matches!(self, SubmitOutcome::MergedExisting(_))
    }
}

/// submitの失敗。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitError {
    /// キューが満杯。
    QueueFull,
    /// シャットダウン済み。
    Shutdown,
}

/// [`WorkerPool::cancel_job`] の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelOutcome {
    /// 未開始ジョブを除去した (Cancelledとして完了通知済み)。
    CancelledQueued,
    /// 実行中ジョブへ協調キャンセルを要求した。
    CancelRequested,
    /// 該当キーのジョブが存在しない。
    NotFound,
}

/// [`WorkerPool::reprioritize`] の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReprioritizeOutcome {
    /// 未開始ジョブの優先度を更新した。
    Updated,
    /// ジョブは存在するが実行中または優先度が同一のため変更なし。
    Unchanged,
    /// 該当キーのジョブが存在しない。
    NotFound,
}

/// プール構成。
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// ワーカースレッド数。
    pub worker_count: usize,
    /// 未開始ジョブのキュー上限 (満杯時は [`SubmitError::QueueFull`])。
    pub queue_capacity: usize,
}

impl Default for PoolConfig {
    fn default() -> Self {
        let parallelism = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        Self {
            worker_count: worker_count_from_env(parallelism),
            queue_capacity: 4096,
        }
    }
}

/// 既定ワーカー数の決定 (WP-16 低スペック計測ノブ)。
/// 環境変数 `GENTO_THUMB_WORKERS` が1..=32の整数なら既定
/// `min(4, available_parallelism)` を上書きする。未設定・範囲外・非数値は
/// 無視して既定へ戻る (起動時のみ読み、実行中の変更は反映しない)。
fn worker_count_from_env(parallelism: usize) -> usize {
    let parsed = parse_worker_override(
        std::env::var_os("GENTO_THUMB_WORKERS")
            .as_deref()
            .and_then(|v| v.to_str()),
    );
    if let Some(n) = parsed {
        tracing::info!(workers = n, "GENTO_THUMB_WORKERS override active");
        n
    } else {
        parallelism.min(4)
    }
}

/// [`worker_count_from_env`] の純粋解析部 (envに触れないので並行テスト安全)。
fn parse_worker_override(value: Option<&str>) -> Option<usize> {
    value?
        .parse::<usize>()
        .ok()
        .filter(|n| (1..=32).contains(n))
}

/// 待機スロット: 完了時に全待機者へ通知する。
#[derive(Debug)]
struct WaitSlot(Mutex<Option<Outcome>>, Condvar);

impl WaitSlot {
    fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(None), Condvar::new()))
    }
}

/// submitが返す完了待ちハンドル。dedup統合時は同一スロットを共有する。
#[derive(Debug, Clone)]
pub struct JobHandle {
    job_id: u64,
    slot: Arc<WaitSlot>,
}

impl PartialEq for JobHandle {
    fn eq(&self, other: &Self) -> bool {
        self.job_id == other.job_id
    }
}

impl Eq for JobHandle {}

impl JobHandle {
    /// 完了までブロックし、結果のクローンを返す。
    pub fn wait(&self) -> Outcome {
        let mut guard = self
            .slot
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            if let Some(outcome) = guard.as_ref() {
                return outcome.clone();
            }
            guard = self
                .slot
                .1
                .wait(guard)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }
}

/// BinaryHeap用エントリ。「大きいほど先に実行」= 優先度値が小さいほど、
/// 同一優先度ならfifo_seqが小さいほど先。
#[derive(Debug, Clone)]
struct HeapEntry {
    entry_seq: u64,
    job_id: u64,
    priority: u8,
    fifo_seq: u64,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.entry_seq == other.entry_seq
    }
}
impl Eq for HeapEntry {}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        // 逆順: 先に実行すべきものを「最大」にする
        other
            .priority
            .cmp(&self.priority)
            .then_with(|| other.fifo_seq.cmp(&self.fifo_seq))
            // tie-breakは一意に (BinaryHeapの安定性に依存しない)
            .then_with(|| other.entry_seq.cmp(&self.entry_seq))
    }
}

struct JobSlot {
    key: String,
    job: Arc<dyn Job>,
    priority: Priority,
    cancel: CancelToken,
    running: bool,
    /// 有効なヒープエントリのentry_seq。再優先度化で差し替わる。
    live_entry: u64,
    wait: Arc<WaitSlot>,
}

#[derive(Default)]
struct QueueState {
    heap: BinaryHeap<HeapEntry>,
    jobs: HashMap<u64, JobSlot>,
    by_key: HashMap<String, u64>,
    entry_seq: u64,
    fifo_seq: u64,
    job_ids: u64,
    /// 未開始ジョブ数 (capacity判定)。
    queued_count: usize,
}

struct PoolInner {
    queue: Mutex<QueueState>,
    available: Condvar,
    shutdown: AtomicBool,
    paused: AtomicBool,
    completions: Sender<JobCompletion>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 上限付き・優先度付きワーカープール。
pub struct WorkerPool {
    inner: Arc<PoolInner>,
    /// shutdown時にtakeしてjoinする。
    workers: Mutex<Vec<JoinHandle<()>>>,
    /// テスト・統計用: ワーカー数。
    worker_count: usize,
    /// 未開始ジョブのキュー上限。
    queue_capacity: usize,
}

impl WorkerPool {
    /// プールを作り、ワーカーを起動する。完了通知の受信側を返す。
    pub fn new(config: PoolConfig) -> (Arc<Self>, Receiver<JobCompletion>) {
        let (tx, rx) = mpsc::channel();
        let inner = Arc::new(PoolInner {
            queue: Mutex::new(QueueState::default()),
            available: Condvar::new(),
            shutdown: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            completions: tx,
        });
        let mut workers = Vec::with_capacity(config.worker_count);
        for _ in 0..config.worker_count {
            let inner = Arc::clone(&inner);
            workers.push(
                thread::Builder::new()
                    .spawn(move || worker_loop(inner))
                    .expect("failed to spawn thumbnail worker (thread limit?)"),
            );
        }
        let pool = Arc::new(Self {
            inner,
            workers: Mutex::new(workers),
            worker_count: config.worker_count,
            queue_capacity: config.queue_capacity,
        });
        (pool, rx)
    }

    pub fn worker_count(&self) -> usize {
        self.worker_count
    }

    /// 未開始ジョブのキュー上限 (再投入側の空き判定に使用)。
    pub fn queue_capacity(&self) -> usize {
        self.queue_capacity
    }

    /// 未開始ジョブ数。
    pub fn queue_depth(&self) -> usize {
        lock(&self.inner.queue).queued_count
    }

    /// 優先度別の未開始ジョブ数 (添字 = `Priority::as_u8()`)。
    /// 開発パネルの「キュー長・優先度別件数」表示 (WP-7 / フェーズ8) 用。
    pub fn queue_depths_by_priority(&self) -> [usize; 5] {
        let queue = lock(&self.inner.queue);
        let mut counts = [0usize; 5];
        for slot in queue.jobs.values() {
            if !slot.running {
                counts[slot.priority.as_u8() as usize] += 1;
            }
        }
        counts
    }

    pub fn is_paused(&self) -> bool {
        self.inner.paused.load(Ordering::SeqCst)
    }

    /// 一時停止/再開。実行中ジョブは完了まで継続する。
    pub fn set_paused(&self, paused: bool) {
        self.inner.paused.store(paused, Ordering::SeqCst);
        self.inner.available.notify_all();
    }

    /// ジョブを投入する。同一キーのジョブが存在する場合は統合される。
    pub fn submit(
        &self,
        key: &str,
        priority: Priority,
        job: Arc<dyn Job>,
    ) -> Result<SubmitOutcome, SubmitError> {
        if self.inner.shutdown.load(Ordering::SeqCst) {
            return Err(SubmitError::Shutdown);
        }
        let handle = {
            let mut queue = lock(&self.inner.queue);
            if let Some(&job_id) = queue.by_key.get(key) {
                let slot = &queue.jobs[&job_id];
                let handle = JobHandle {
                    job_id,
                    slot: Arc::clone(&slot.wait),
                };
                return Ok(SubmitOutcome::MergedExisting(handle));
            }
            if queue.queued_count >= self.capacity() {
                return Err(SubmitError::QueueFull);
            }
            queue.job_ids += 1;
            queue.fifo_seq += 1;
            queue.entry_seq += 1;
            let job_id = queue.job_ids;
            let entry_seq = queue.entry_seq;
            let wait = WaitSlot::new();
            queue.jobs.insert(
                job_id,
                JobSlot {
                    key: key.to_string(),
                    job,
                    priority,
                    cancel: CancelToken::new(),
                    running: false,
                    live_entry: entry_seq,
                    wait: Arc::clone(&wait),
                },
            );
            queue.by_key.insert(key.to_string(), job_id);
            let fifo_seq = queue.fifo_seq;
            queue.heap.push(HeapEntry {
                entry_seq,
                job_id,
                priority: priority.as_u8(),
                fifo_seq,
            });
            queue.queued_count += 1;
            JobHandle { job_id, slot: wait }
        };
        self.inner.available.notify_one();
        Ok(SubmitOutcome::Enqueued(handle))
    }

    fn capacity(&self) -> usize {
        self.queue_capacity
    }

    /// 未開始ジョブの優先度を更新する。停滞した古いヒープエントリは
    /// pop時のlive_entry照合で無効化される。
    pub fn reprioritize(&self, key: &str, priority: Priority) -> ReprioritizeOutcome {
        let mut queue = lock(&self.inner.queue);
        let Some(&job_id) = queue.by_key.get(key) else {
            return ReprioritizeOutcome::NotFound;
        };
        let Some(slot) = queue.jobs.get(&job_id) else {
            return ReprioritizeOutcome::NotFound;
        };
        if slot.running || slot.priority == priority {
            return ReprioritizeOutcome::Unchanged;
        }
        let fifo_seq = queue
            .heap
            .iter()
            .find(|e| e.job_id == job_id)
            .map(|e| e.fifo_seq)
            .unwrap_or(0);
        // 不変借用がここで切れるので、以降は可変操作を行える
        queue.entry_seq += 1;
        let entry_seq = queue.entry_seq;
        let slot = queue
            .jobs
            .get_mut(&job_id)
            .expect("job exists (checked above; queue lock held throughout)");
        slot.priority = priority;
        slot.live_entry = entry_seq;
        queue.heap.push(HeapEntry {
            entry_seq,
            job_id,
            priority: priority.as_u8(),
            fifo_seq,
        });
        drop(queue);
        self.inner.available.notify_one();
        ReprioritizeOutcome::Updated
    }

    /// ジョブをキャンセルする。未開始なら即時にCancelledとして完了させ、
    /// 実行中なら協調キャンセルを要求する (ジョブが応答するかは任意)。
    pub fn cancel_job(&self, key: &str) -> CancelOutcome {
        let (outcome, wait) = {
            let mut queue = lock(&self.inner.queue);
            let Some(&job_id) = queue.by_key.get(key) else {
                return CancelOutcome::NotFound;
            };
            let is_running = queue
                .jobs
                .get(&job_id)
                .map(|slot| slot.running)
                .unwrap_or(false);
            if is_running {
                queue
                    .jobs
                    .get_mut(&job_id)
                    .expect("job exists (is_running checked)")
                    .cancel
                    .cancel();
                return CancelOutcome::CancelRequested;
            }
            let slot = queue
                .jobs
                .remove(&job_id)
                .expect("by_key entry must reference a live job slot");
            if queue
                .by_key
                .get(&slot.key)
                .is_some_and(|&kid| kid == job_id)
            {
                queue.by_key.remove(&slot.key);
            }
            queue.queued_count = queue.queued_count.saturating_sub(1);
            (Outcome::Cancelled, slot.wait)
        };
        *wait.0.lock().unwrap_or_else(|p| p.into_inner()) = Some(outcome);
        wait.1.notify_all();
        let _ = self.inner.completions.send(JobCompletion {
            key: key.to_string(),
            outcome: OutcomeSummary::Cancelled,
        });
        CancelOutcome::CancelledQueued
    }

    /// シャットダウン: 待機中ジョブをCancelledとして完了させ、ワーカーをjoinする。
    /// ワーカースレッド内から呼んではならない (自己joinする)。
    pub fn shutdown(&self) {
        self.inner.shutdown.store(true, Ordering::SeqCst);
        self.inner.available.notify_all();
        {
            let mut queue = lock(&self.inner.queue);
            let queued_ids: Vec<u64> = queue
                .jobs
                .iter()
                .filter(|(_, slot)| !slot.running)
                .map(|(id, _)| *id)
                .collect();
            for id in queued_ids {
                let slot = queue.jobs.remove(&id).expect("id collected from map");
                if queue.by_key.get(&slot.key).is_some_and(|&kid| kid == id) {
                    queue.by_key.remove(&slot.key);
                }
                queue.queued_count = queue.queued_count.saturating_sub(1);
                *slot.wait.0.lock().unwrap_or_else(|p| p.into_inner()) = Some(Outcome::Cancelled);
                slot.wait.1.notify_all();
                let _ = self.inner.completions.send(JobCompletion {
                    key: slot.key.clone(),
                    outcome: OutcomeSummary::Cancelled,
                });
            }
            queue.heap.clear();
        }
        let workers: Vec<JoinHandle<()>> = std::mem::take(&mut *lock(&self.workers));
        for worker in workers {
            let _ = worker.join();
        }
    }
}

impl PoolInner {
    /// ジョブ完了の確定: 待機スロットへ通知し、完了チャネルへ流す。
    fn complete(&self, job_id: u64, key: String, outcome: Outcome) {
        let summary = outcome.summary();
        {
            let mut queue = lock(&self.queue);
            if let Some(slot) = queue.jobs.remove(&job_id) {
                if queue
                    .by_key
                    .get(&slot.key)
                    .is_some_and(|&kid| kid == job_id)
                {
                    queue.by_key.remove(&slot.key);
                }
                *slot.wait.0.lock().unwrap_or_else(|p| p.into_inner()) = Some(outcome);
                slot.wait.1.notify_all();
            } else {
                // cancel_job/shutdownが先に除去済み。完了通知だけ流す。
                tracing::debug!(job_id, key = %key, "completion for already-removed job");
            }
        }
        let _ = self.completions.send(JobCompletion {
            key,
            outcome: summary,
        });
    }
}

/// ワーカーループ。ロックは取得・判定のみで、ジョブ実行はロック外。
fn worker_loop(inner: Arc<PoolInner>) {
    loop {
        let picked = {
            let mut queue = lock(&inner.queue);
            loop {
                if inner.shutdown.load(Ordering::SeqCst) {
                    return;
                }
                // 停滞エントリ (再優先度化・キャンセル済み) を掃きながら先頭を探す
                while let Some(top) = queue.heap.peek() {
                    let valid = queue
                        .jobs
                        .get(&top.job_id)
                        .map(|slot| !slot.running && slot.live_entry == top.entry_seq)
                        .unwrap_or(false);
                    if valid {
                        break;
                    }
                    queue.heap.pop();
                }
                if !inner.paused.load(Ordering::SeqCst) && queue.heap.peek().is_some() {
                    let entry = queue.heap.pop().expect("peek checked above");
                    let (job, cancel, key) = {
                        let slot = queue
                            .jobs
                            .get_mut(&entry.job_id)
                            .expect("validated heap entries always reference a queued job");
                        slot.running = true;
                        (Arc::clone(&slot.job), slot.cancel.clone(), slot.key.clone())
                    };
                    queue.queued_count = queue.queued_count.saturating_sub(1);
                    break Some((entry.job_id, job, cancel, key));
                }
                // 実行可能ジョブなし。通知またはタイムアウトで再検査する
                // (タイムアウトは紛失したnotifyに対する保険)。
                let (guard, _) = inner
                    .available
                    .wait_timeout(queue, Duration::from_millis(200))
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                queue = guard;
            }
        };
        let Some((job_id, job, cancel, key)) = picked else {
            return;
        };
        let result = panic::catch_unwind(AssertUnwindSafe(|| job.run(&cancel)));
        let outcome = match result {
            Ok(Ok(bytes)) => Outcome::Completed(bytes),
            Ok(Err(reason)) => {
                if cancel.is_cancelled() {
                    Outcome::Cancelled
                } else {
                    Outcome::Failed(reason)
                }
            }
            Err(panic_payload) => Outcome::Failed(panic_message(&panic_payload)),
        };
        inner.complete(job_id, key, outcome);
    }
}

/// panicペイロードからメッセージ文字列を取り出す。
fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        format!("panic: {s}")
    } else if let Some(s) = payload.downcast_ref::<String>() {
        format!("panic: {s}")
    } else {
        "panic: unknown payload".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// WP-16: GENTO_THUMB_WORKERS の解析 (純粋関数なのでenv競合なし)。
    #[test]
    fn worker_override_parses_only_bounded_positive_ints() {
        assert_eq!(parse_worker_override(Some("1")), Some(1));
        assert_eq!(parse_worker_override(Some("2")), Some(2));
        assert_eq!(parse_worker_override(Some("32")), Some(32));
        // 0・上限超過・非数値・空・前後空白はすべて無視 (既定へ戻る)
        assert_eq!(parse_worker_override(Some("0")), None);
        assert_eq!(parse_worker_override(Some("33")), None);
        assert_eq!(parse_worker_override(Some("-1")), None);
        assert_eq!(parse_worker_override(Some("abc")), None);
        assert_eq!(parse_worker_override(Some("")), None);
        assert_eq!(parse_worker_override(Some(" 2")), None);
        assert_eq!(parse_worker_override(None), None);
    }

    /// 既定経路: env未設定なら min(4, parallelism) のまま。
    /// (worker_count_from_env 自体はenvを読むため、ここは引数で既定を検証)
    #[test]
    fn worker_count_default_is_min4_when_override_absent() {
        // このテストはenv GENTO_THUMB_WORKERS を設定せずに実行される前提。
        // 計測ドライバが当環境変数をexportしていない状態で実行すること。
        let resolved = worker_count_from_env(20);
        if std::env::var_os("GENTO_THUMB_WORKERS").is_some() {
            return; // 外部からoverrideされた環境では検証しない
        }
        assert_eq!(resolved, 4);
        assert_eq!(worker_count_from_env(2), 2);
    }

    /// 即時に成功するジョブ。
    struct OkJob {
        runs: Arc<AtomicUsize>,
    }

    impl Job for OkJob {
        fn run(&self, _cancel: &CancelToken) -> Result<Vec<u8>, String> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            Ok(vec![1, 2, 3])
        }
    }

    /// 開始を報告し、解放信号まで回るブロッキングジョブ (キャンセル也应答)。
    /// ReceiverはSyncでないためMutexで包む。
    struct BlockerJob {
        runs: Arc<AtomicUsize>,
        started: Sender<()>,
        release: Mutex<Receiver<()>>,
    }

    impl Job for BlockerJob {
        fn run(&self, cancel: &CancelToken) -> Result<Vec<u8>, String> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            let _ = self.started.send(());
            loop {
                if cancel.is_cancelled() {
                    return Err("cancelled".to_string());
                }
                if self
                    .release
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .try_recv()
                    .is_ok()
                {
                    break;
                }
                thread::sleep(Duration::from_millis(2));
            }
            Ok(vec![9])
        }
    }

    /// キャンセルされるまでループするジョブ。
    struct PollCancelJob {
        runs: Arc<AtomicUsize>,
    }

    impl Job for PollCancelJob {
        fn run(&self, cancel: &CancelToken) -> Result<Vec<u8>, String> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            while !cancel.is_cancelled() {
                thread::sleep(Duration::from_millis(2));
            }
            Err("cancelled".to_string())
        }
    }

    /// panicするジョブ。
    struct PanicJob;

    impl Job for PanicJob {
        fn run(&self, _cancel: &CancelToken) -> Result<Vec<u8>, String> {
            panic!("boom");
        }
    }

    /// 失敗するジョブ。
    struct ErrJob;

    impl Job for ErrJob {
        fn run(&self, _cancel: &CancelToken) -> Result<Vec<u8>, String> {
            Err("decode failed".to_string())
        }
    }

    fn pool(workers: usize, capacity: usize) -> (Arc<WorkerPool>, Receiver<JobCompletion>) {
        WorkerPool::new(PoolConfig {
            worker_count: workers,
            queue_capacity: capacity,
        })
    }

    /// `expected`件の完了を受信するかタイムアウトまでドレインする。
    /// waitスロット通知と完了チャネル送信の間のわずかな遅延に耐えるため必須。
    fn drain_all(rx: &Receiver<JobCompletion>, expected: usize) -> Vec<JobCompletion> {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut out = Vec::new();
        while out.len() < expected {
            if std::time::Instant::now() > deadline {
                break;
            }
            match rx.try_recv() {
                Ok(c) => out.push(c),
                Err(_) => thread::sleep(Duration::from_millis(2)),
            }
        }
        out
    }

    fn completions_by_key(
        rx: &Receiver<JobCompletion>,
        key: &str,
        total_expected: usize,
    ) -> Vec<OutcomeSummary> {
        drain_all(rx, total_expected)
            .into_iter()
            .filter(|c| c.key == key)
            .map(|c| c.outcome)
            .collect()
    }

    #[test]
    fn priority_order_then_fifo_within_priority() {
        let (p, rx) = pool(1, 16);
        let runs = Arc::new(AtomicUsize::new(0));
        let submit = |key: &str, priority: Priority| {
            p.submit(
                key,
                priority,
                Arc::new(OkJob {
                    runs: Arc::clone(&runs),
                }),
            )
            .unwrap()
            .handle()
        };
        let h_bg1 = submit("bg1", Priority::Background);
        let h_ia = submit("interactive", Priority::Interactive);
        let h_bg2 = submit("bg2", Priority::Background);
        let h_vp = submit("viewport", Priority::Viewport);

        assert_eq!(h_ia.wait(), Outcome::Completed(vec![1, 2, 3]));
        assert_eq!(h_vp.wait(), Outcome::Completed(vec![1, 2, 3]));
        assert_eq!(h_bg1.wait(), Outcome::Completed(vec![1, 2, 3]));
        assert_eq!(h_bg2.wait(), Outcome::Completed(vec![1, 2, 3]));
        assert_eq!(runs.load(Ordering::SeqCst), 4);

        // 完了順 (completion受信順) でも検証: interactive → viewport → bg1 → bg2
        let order: Vec<String> = drain_all(&rx, 4).into_iter().map(|c| c.key).collect();
        assert_eq!(order, vec!["interactive", "viewport", "bg1", "bg2"]);
        p.shutdown();
    }

    #[test]
    fn queue_depths_by_priority_counts_only_queued_jobs() {
        let (p, _rx) = pool(1, 64);
        p.set_paused(true);
        let runs = Arc::new(AtomicUsize::new(0));
        for (key, priority) in [
            ("i1", Priority::Interactive),
            ("i2", Priority::Interactive),
            ("v1", Priority::Viewport),
            ("b1", Priority::Background),
            ("b2", Priority::Background),
            ("b3", Priority::Background),
        ] {
            p.submit(
                key,
                priority,
                Arc::new(OkJob {
                    runs: Arc::clone(&runs),
                }),
            )
            .unwrap();
        }
        let depths = p.queue_depths_by_priority();
        assert_eq!(depths[Priority::Interactive.as_u8() as usize], 2);
        assert_eq!(depths[Priority::Viewport.as_u8() as usize], 1);
        assert_eq!(depths[Priority::Prefetch.as_u8() as usize], 0);
        assert_eq!(depths[Priority::Overscan.as_u8() as usize], 0);
        assert_eq!(depths[Priority::Background.as_u8() as usize], 3);
        assert_eq!(depths.iter().sum::<usize>(), p.queue_depth());

        // 再優先度化はカウントにも反映される
        assert_eq!(
            p.reprioritize("b1", Priority::Interactive),
            ReprioritizeOutcome::Updated
        );
        let depths = p.queue_depths_by_priority();
        assert_eq!(depths[Priority::Interactive.as_u8() as usize], 3);
        assert_eq!(depths[Priority::Background.as_u8() as usize], 2);

        // キャンセルで減る
        p.cancel_job("i2");
        let depths = p.queue_depths_by_priority();
        assert_eq!(depths[Priority::Interactive.as_u8() as usize], 2);

        // 実行開始でキューから減る (1ワーカー・一時停止解除で即開始)
        p.set_paused(false);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while p.queue_depth() > 0 && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(p.queue_depths_by_priority().iter().sum::<usize>(), 0);
        p.shutdown();
    }

    #[test]
    fn same_key_requests_merge_into_single_execution() {
        let (p, rx) = pool(1, 16);
        let runs = Arc::new(AtomicUsize::new(0));
        let (start_tx, start_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let first = p
            .submit(
                "k",
                Priority::Background,
                Arc::new(BlockerJob {
                    runs: Arc::clone(&runs),
                    started: start_tx,
                    release: Mutex::new(release_rx),
                }),
            )
            .unwrap()
            .handle();
        start_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        let mut handles = vec![first];
        for _ in 0..5 {
            match p
                .submit(
                    "k",
                    Priority::Background,
                    Arc::new(OkJob {
                        runs: Arc::clone(&runs),
                    }),
                )
                .unwrap()
            {
                SubmitOutcome::MergedExisting(h) => handles.push(h),
                SubmitOutcome::Enqueued(_) => panic!("duplicate key must merge"),
            }
        }
        release_tx.send(()).unwrap();
        for handle in &handles {
            assert_eq!(handle.wait(), Outcome::Completed(vec![9]));
        }
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "dedup: exactly one execution"
        );
        // 完了通知はジョブごとに1回だけ
        let summaries = completions_by_key(&rx, "k", 1);
        assert_eq!(summaries, vec![OutcomeSummary::Completed]);
        p.shutdown();
    }

    #[test]
    fn cancel_unstarted_removes_without_running() {
        let (p, rx) = pool(1, 16);
        let runs = Arc::new(AtomicUsize::new(0));
        let (start_tx, start_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let _blocker = p
            .submit(
                "blocker",
                Priority::Interactive,
                Arc::new(BlockerJob {
                    runs: Arc::clone(&runs),
                    started: start_tx,
                    release: Mutex::new(release_rx),
                }),
            )
            .unwrap();
        start_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        let victim = p
            .submit(
                "victim",
                Priority::Background,
                Arc::new(OkJob {
                    runs: Arc::clone(&runs),
                }),
            )
            .unwrap();
        assert_eq!(p.cancel_job("victim"), CancelOutcome::CancelledQueued);
        match victim {
            SubmitOutcome::Enqueued(h) | SubmitOutcome::MergedExisting(h) => {
                assert_eq!(h.wait(), Outcome::Cancelled);
            }
        }
        release_tx.send(()).unwrap();
        // victimは一度も実行されていない
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        let summaries = completions_by_key(&rx, "victim", 2);
        assert_eq!(summaries, vec![OutcomeSummary::Cancelled]);
        p.shutdown();
    }

    #[test]
    fn cancel_running_requests_cooperative_cancel() {
        let (p, rx) = pool(1, 16);
        let runs = Arc::new(AtomicUsize::new(0));
        let handle = match p
            .submit(
                "poll",
                Priority::Interactive,
                Arc::new(PollCancelJob {
                    runs: Arc::clone(&runs),
                }),
            )
            .unwrap()
        {
            SubmitOutcome::Enqueued(h) => h,
            SubmitOutcome::MergedExisting(_) => panic!("fresh key"),
        };
        // ジョブ開始を待つ (実行中になったらCancelRequested)
        loop {
            if p.queue_depth() == 0 && runs.load(Ordering::SeqCst) == 1 {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(p.cancel_job("poll"), CancelOutcome::CancelRequested);
        assert_eq!(handle.wait(), Outcome::Cancelled);
        let summaries = completions_by_key(&rx, "poll", 1);
        assert_eq!(summaries, vec![OutcomeSummary::Cancelled]);
        p.shutdown();
    }

    #[test]
    fn queue_full_rejects_but_merge_is_allowed() {
        let (p, _rx) = pool(1, 2);
        let runs = Arc::new(AtomicUsize::new(0));
        let (start_tx, start_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let _blocker = p
            .submit(
                "blocker",
                Priority::Interactive,
                Arc::new(BlockerJob {
                    runs: Arc::clone(&runs),
                    started: start_tx,
                    release: Mutex::new(release_rx),
                }),
            )
            .unwrap();
        start_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        let ok = OkJob {
            runs: Arc::clone(&runs),
        };
        let _q1 = p
            .submit(
                "q1",
                Priority::Background,
                Arc::new(OkJob {
                    runs: Arc::clone(&runs),
                }),
            )
            .unwrap();
        let _q2 = p
            .submit(
                "q2",
                Priority::Background,
                Arc::new(OkJob {
                    runs: Arc::clone(&runs),
                }),
            )
            .unwrap();
        assert_eq!(p.queue_depth(), 2);
        // 満杯 (capacity=2) → 拒否
        assert_eq!(
            p.submit("q3", Priority::Background, Arc::new(ok)),
            Err(SubmitError::QueueFull)
        );
        // 既存キーへの統合は満杯でも許可される
        assert!(matches!(
            p.submit(
                "q1",
                Priority::Background,
                Arc::new(OkJob {
                    runs: Arc::clone(&runs)
                })
            ),
            Ok(SubmitOutcome::MergedExisting(_))
        ));
        release_tx.send(()).unwrap();
        p.shutdown();
    }

    #[test]
    fn pause_blocks_new_starts_and_resume_continues() {
        let (p, _rx) = pool(2, 16);
        let runs = Arc::new(AtomicUsize::new(0));
        p.set_paused(true);
        assert!(p.is_paused());
        let h1 = match p
            .submit(
                "a",
                Priority::Background,
                Arc::new(OkJob {
                    runs: Arc::clone(&runs),
                }),
            )
            .unwrap()
        {
            SubmitOutcome::Enqueued(h) => h,
            SubmitOutcome::MergedExisting(_) => panic!(),
        };
        thread::sleep(Duration::from_millis(100));
        assert_eq!(
            runs.load(Ordering::SeqCst),
            0,
            "paused pool must not start jobs"
        );
        p.set_paused(false);
        assert_eq!(h1.wait(), Outcome::Completed(vec![1, 2, 3]));
        p.shutdown();
    }

    #[test]
    fn panicking_job_is_isolated_and_pool_continues() {
        let (p, rx) = pool(1, 16);
        let runs = Arc::new(AtomicUsize::new(0));
        let h_panic = match p
            .submit("panic", Priority::Interactive, Arc::new(PanicJob))
            .unwrap()
        {
            SubmitOutcome::Enqueued(h) => h,
            SubmitOutcome::MergedExisting(_) => panic!(),
        };
        match h_panic.wait() {
            Outcome::Failed(reason) => assert!(reason.contains("boom"), "{reason}"),
            other => panic!("expected failure, got {other:?}"),
        }
        let h_ok = match p
            .submit(
                "ok",
                Priority::Interactive,
                Arc::new(OkJob {
                    runs: Arc::clone(&runs),
                }),
            )
            .unwrap()
        {
            SubmitOutcome::Enqueued(h) => h,
            SubmitOutcome::MergedExisting(_) => panic!(),
        };
        assert_eq!(h_ok.wait(), Outcome::Completed(vec![1, 2, 3]));
        let summaries = completions_by_key(&rx, "panic", 2);
        assert!(matches!(summaries[0], OutcomeSummary::Failed(_)));
        p.shutdown();
    }

    #[test]
    fn err_job_completes_as_failed() {
        let (p, rx) = pool(1, 16);
        let h = match p
            .submit("err", Priority::Interactive, Arc::new(ErrJob))
            .unwrap()
        {
            SubmitOutcome::Enqueued(h) => h,
            SubmitOutcome::MergedExisting(_) => panic!(),
        };
        assert_eq!(h.wait(), Outcome::Failed("decode failed".to_string()));
        assert_eq!(
            completions_by_key(&rx, "err", 1),
            vec![OutcomeSummary::Failed("decode failed".to_string())]
        );
        p.shutdown();
    }

    #[test]
    fn shutdown_cancels_queued_and_joins_workers() {
        let (p, rx) = pool(2, 16);
        let runs = Arc::new(AtomicUsize::new(0));
        p.set_paused(true); // 何も開始させない (決定的にする)
        let handles: Vec<JobHandle> = (0..3)
            .map(|i| {
                match p
                    .submit(
                        &format!("job{i}"),
                        Priority::Background,
                        Arc::new(OkJob {
                            runs: Arc::clone(&runs),
                        }),
                    )
                    .unwrap()
                {
                    SubmitOutcome::Enqueued(h) => h,
                    SubmitOutcome::MergedExisting(_) => panic!(),
                }
            })
            .collect();
        p.shutdown();
        for h in &handles {
            assert_eq!(h.wait(), Outcome::Cancelled);
        }
        assert_eq!(runs.load(Ordering::SeqCst), 0);
        let cancelled = drain_all(&rx, 3);
        assert!(cancelled
            .iter()
            .all(|c| c.outcome == OutcomeSummary::Cancelled));
        assert_eq!(cancelled.len(), 3);
        // シャットダウン後のsubmitは拒否
        assert_eq!(
            p.submit(
                "late",
                Priority::Background,
                Arc::new(OkJob {
                    runs: Arc::clone(&runs),
                })
            ),
            Err(SubmitError::Shutdown)
        );
    }

    #[test]
    fn reprioritize_pending_job_changes_execution_order() {
        let (p, rx) = pool(1, 16);
        let runs = Arc::new(AtomicUsize::new(0));
        let (start_tx, start_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let _blocker = p
            .submit(
                "blocker",
                Priority::Interactive,
                Arc::new(BlockerJob {
                    runs: Arc::clone(&runs),
                    started: start_tx,
                    release: Mutex::new(release_rx),
                }),
            )
            .unwrap();
        start_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        let ha = match p
            .submit(
                "a",
                Priority::Background,
                Arc::new(OkJob {
                    runs: Arc::clone(&runs),
                }),
            )
            .unwrap()
        {
            SubmitOutcome::Enqueued(h) => h,
            SubmitOutcome::MergedExisting(_) => panic!(),
        };
        let hb = match p
            .submit(
                "b",
                Priority::Background,
                Arc::new(OkJob {
                    runs: Arc::clone(&runs),
                }),
            )
            .unwrap()
        {
            SubmitOutcome::Enqueued(h) => h,
            SubmitOutcome::MergedExisting(_) => panic!(),
        };
        // bを先頭へ引き上げ
        assert_eq!(
            p.reprioritize("b", Priority::Interactive),
            ReprioritizeOutcome::Updated
        );
        release_tx.send(()).unwrap();
        // 完了順: blocker → b → a
        assert_eq!(hb.wait(), Outcome::Completed(vec![1, 2, 3]));
        assert_eq!(ha.wait(), Outcome::Completed(vec![1, 2, 3]));
        let order: Vec<String> = drain_all(&rx, 3).into_iter().map(|c| c.key).collect();
        assert_eq!(order, vec!["blocker", "b", "a"]);
        p.shutdown();
    }

    #[test]
    fn reprioritize_missing_key_is_not_found() {
        let (p, _rx) = pool(1, 16);
        assert_eq!(
            p.reprioritize("nope", Priority::Interactive),
            ReprioritizeOutcome::NotFound
        );
        p.shutdown();
    }

    #[test]
    fn concurrent_submits_all_complete() {
        let (p, rx) = pool(4, 4096);
        let runs = Arc::new(AtomicUsize::new(0));
        thread::scope(|scope| {
            let mut joins = Vec::new();
            for t in 0..8 {
                let p = Arc::clone(&p);
                let runs = Arc::clone(&runs);
                joins.push(scope.spawn(move || {
                    for i in 0..25 {
                        let h = p
                            .submit(
                                &format!("t{t}-i{i}"),
                                Priority::Background,
                                Arc::new(OkJob {
                                    runs: Arc::clone(&runs),
                                }),
                            )
                            .unwrap();
                        if i % 7 == 0 {
                            // 一部だけ同期待ち (残りは完了チャネルで数える)
                            h.handle().wait();
                        }
                    }
                }));
            }
            for j in joins {
                j.join().unwrap();
            }
        });
        // 全200ジョブの完了を受け取る
        let completed = drain_all(&rx, 200);
        assert!(completed
            .iter()
            .all(|c| c.outcome == OutcomeSummary::Completed));
        assert_eq!(completed.len(), 200);
        assert_eq!(runs.load(Ordering::SeqCst), 200);
        p.shutdown();
    }
}
