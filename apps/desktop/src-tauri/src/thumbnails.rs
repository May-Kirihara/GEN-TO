//! サムネイル生成サービスとcommand群 (Phase 4 / WP-3)。
//!
//! gento-image (pool/cache/stats) を組み合わせたTauri thin adapter。
//! - 要求ごとにキャッシュlookup → ヒットなら即時 `thumb:ready`
//! - ミスならプールへ投入 (dedup・優先度・上限付き)
//! - 完了通知はバッチング (100ms or 20件) して `thumb:ready` / `thumb:failed`
//! - 統計・一時停止・キャッシュ全削除 (cache_generation更新)
//!
//! 契約: docs/dev/api-contract.md §7以降。

use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gento_core::{AssetId, Priority, SizeClass};
use gento_image::cache::{asset_fingerprint, cache_key, CacheLookup, ThumbnailCache};
use gento_image::pool::{OutcomeSummary, PoolConfig, SubmitError, WorkerPool};
use gento_image::stats::ThumbStats;
#[cfg(any(debug_assertions, feature = "devtools"))]
use gento_image::thumbnail::FailureInjection;
#[cfg(any(debug_assertions, feature = "devtools"))]
use gento_image::thumbnail::InjectionMode;
use gento_image::thumbnail::ThumbnailJob;
use serde::Serialize;
use tauri::{Emitter, State};

use crate::catalog::AppState;

/// イベントバッチの最大件数。
const EVENT_BATCH_MAX: usize = 20;
/// バッチ窓口時間。
const EVENT_BATCH_WINDOW: Duration = Duration::from_millis(100);
/// 完了イベント集計ログの間隔 (イベント嵐対策のinfoログ)。
const EMITTER_SUMMARY_EVERY: u64 = 500;
/// backlog再投入の空きマージン (capacity - depth がこれ未満なら再投入しない)。
/// library.rsのハッシュ再提出 (HASH_RESUBMIT_MARGIN) と同じ値・同じ意図
/// (backlogがキューを埋め尽くしてUI由来了のheadroomを奪わない)。
const BACKLOG_SUBMIT_MARGIN: usize = 64;
/// 1回の起床で纏めて再投入する件数上限 (ハッシュ再提出と同じ値)。
const BACKLOG_SUBMIT_BATCH: usize = 256;

/// `thumb:ready` の項目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ThumbReadyItem {
    pub asset_id: AssetId,
    pub size_class: String,
    pub cache_generation: u64,
    /// 内容感応URLバージョン (FG-1 / F3)。フロントは `?v={version}` 付き
    /// プロトコルURLを組み立てる。元ファイル (size/mtime) 変化で変わる。
    pub version: u64,
}

/// `thumb:failed` の項目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ThumbFailedItem {
    pub asset_id: AssetId,
    pub size_class: String,
    pub reason: String,
}

/// サービスからUIへのイベント出口。実装はTauri emit、試験は記録器。
pub trait ThumbEvents: Send {
    fn ready(&self, items: &[ThumbReadyItem]);
    fn failed(&self, items: &[ThumbFailedItem]);
}

/// 要求解決に必要なカタログスナップショット項目。
#[derive(Debug, Clone)]
pub struct AssetSnapshot {
    pub id: AssetId,
    pub source_path: PathBuf,
    pub byte_size: u64,
    pub mtime_ms: u64,
    /// ImageState::Ready相当か (デコード不能なレコードは要求を棄却)。
    pub readable: bool,
    /// プローブ由来のMIME (original応答のContent-Type。WP-3)。
    pub content_type: String,
}

/// 要求の受理内訳。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
pub struct RequestAccepted {
    pub queued: u64,
    /// 既存ジョブへの統合 (2回目以降の同一キー要求)。
    pub rejected_dup: u64,
    pub rejected_full: u64,
    /// キュー満杯でbacklogへ繰延された件数 (generate_allのみ。完了駆動で
    /// 空きができ次第投入される。WP-16 F1)。
    pub deferred: u64,
    /// キャッシュヒット (即時ready。キューに入らない)。
    pub cache_hits: u64,
    /// state != Ready のレコード。
    pub skipped_unreadable: u64,
    /// カタログに存在しないID。
    pub skipped_unknown: u64,
}

/// 1アセット分の投入結果 ([`ThumbnailService::request`] とbacklog feederの
/// 共通経路 [`ThumbnailCore::enqueue_asset`] の戻り値)。
#[derive(Debug)]
enum EnqueueOutcome {
    /// 新規ジョブとして投入された。
    Queued,
    /// 既存ジョブへの統合 (dedup)。
    Merged,
    /// キャッシュヒット (即時ready。呼び出し側で `ImmediateReady` へ集約)。
    CacheHit(ThumbReadyItem),
    /// キュー満杯。
    Full,
    /// プール停止済み。
    Shutdown,
}

/// 全件生成のbacklog項目 (キュー上限で入れ切れなかった残り)。
/// `generation` は投入元の `request_all` 世代 (FG-WP16-3 B3)。
#[derive(Debug, Clone)]
struct BacklogItem {
    generation: u64,
    asset: AssetSnapshot,
    size_class: SizeClass,
    priority: Priority,
}

/// backlogの状態 (drain報告用カウンタ込み)。
#[derive(Debug, Default)]
struct BacklogState {
    items: VecDeque<BacklogItem>,
    /// このbacklogの属する `request_all` 世代 (単調増加トークン。
    /// `request_generation` カウンタ由来 — 0は初期/clear後)。
    generation: u64,
    /// 現在の要求で繰延された総数 (報告用。置換時にリセット)。
    deferred_total: u64,
    /// 現在の要求でbacklogから処理 (投入・統合・フィード時ヒット) した数。
    processed: u64,
    /// feederがpop済み・まだプールへ投入していないitem (FG-WP16-4 R1)。
    /// pop時 (backlogロック内) に登録され、投入判定 (同ロック内) で消費
    /// される。この間にcancelが一致キーを破棄した場合はNoneへ差し替えられ、
    /// feederは投入判定で当該itemを廃棄する — pop→submit窓にcancelの
    /// 死角が構造的に存在しない。
    inflight: Option<InflightItem>,
    /// inflight識別子の供給 (単調増加。feederは単一スレッドのためABAなし)。
    pop_seq: u64,
}

/// feeder処理中のbacklog itemの追跡 (cancel照合に使用。世代検査は
/// feederが手持ちのitem自体の世代で行うためここには保持しない)。
#[derive(Debug, Clone)]
struct InflightItem {
    pop_id: u64,
    /// cancel照合用のキャッシュキー (pop時に算出)。
    key: String,
}

impl BacklogState {
    /// 先頭itemをfeeder処理中として取り出す (backlogロック内で呼ぶこと)。
    /// popと同時にinflightへ登録するため、この直後から投入判定までの間も
    /// cancelはこのitemを観測・破棄できる (FG-WP16-4 R1)。
    fn pop_for_feeder(&mut self) -> Option<(u64, BacklogItem)> {
        let item = self.items.pop_front()?;
        self.pop_seq += 1;
        let pop_id = self.pop_seq;
        let key = cache_key(
            &asset_fingerprint(
                &item.asset.source_path,
                item.asset.byte_size,
                item.asset.mtime_ms,
            ),
            item.size_class,
        )
        .hex;
        self.inflight = Some(InflightItem { pop_id, key });
        Some((pop_id, item))
    }
}

/// エミッタスレッドへのメッセージ。
enum EmitterMsg {
    JobDone {
        key: String,
        outcome: OutcomeSummary,
    },
    ImmediateReady {
        items: Vec<ThumbReadyItem>,
    },
}

/// 待機中要求の台帳エントリ (ジョブキー → 要求者と要求時の文脈)。
#[derive(Debug, Default)]
struct PendingRequest {
    assets: Vec<AssetId>,
    generation: u64,
    /// このジョブのキャッシュキーに対応する内容感応バージョン。
    /// 同一キーへ統合された要求者は全員同じ値を通知される。
    version: u64,
    /// イベント送出時に埋めるサイズクラスIPC名。
    size_class: String,
}

/// 要求受け付けの共通状態 (サービス本体とbacklog feederスレッドで共有)。
struct ThumbnailCore {
    cache: Arc<ThumbnailCache>,
    pool: Arc<WorkerPool>,
    stats: Arc<ThumbStats>,
    /// ジョブキー → 要求中アセット。完了 (ready/failed/cancelled) で消える。
    registry: Arc<Mutex<HashMap<String, PendingRequest>>>,
    cache_generation: AtomicU64,
    /// 開発用エラー注入 (WP-7)。全ジョブが共有する。devtoolsビルドのみ (WP-0)。
    #[cfg(any(debug_assertions, feature = "devtools"))]
    injection: Arc<FailureInjection>,
    /// テスト用: feederのpop直後・投入直前で一時停止するゲート
    /// (決定論的インターリーブ再現。FG-WP16-4)。
    #[cfg(any(debug_assertions, feature = "devtools"))]
    feeder_gate: Arc<FeederGate>,
    emitter_tx: Sender<EmitterMsg>,
}

impl ThumbnailCore {
    fn cache_generation(&self) -> u64 {
        self.cache_generation.load(Ordering::SeqCst)
    }

    /// 1アセットの要求経路: ヒットは即時ready、ミスはジョブ投入 (dedup)。
    /// [`ThumbnailService::request`] (UI由来・満杯は即時拒否) とbacklog
    /// feeder (満杯は再延期) の共通本体。
    fn enqueue_asset(
        &self,
        asset: &AssetSnapshot,
        size_class: SizeClass,
        priority: Priority,
    ) -> EnqueueOutcome {
        let fingerprint = asset_fingerprint(&asset.source_path, asset.byte_size, asset.mtime_ms);
        let key = cache_key(&fingerprint, size_class);
        let version = content_version_of(&fingerprint);
        match self.cache.lookup(&key) {
            CacheLookup::Hit(_) => {
                self.stats.record_hit();
                EnqueueOutcome::CacheHit(ThumbReadyItem {
                    asset_id: asset.id,
                    size_class: size_class.ipc_name().to_string(),
                    cache_generation: self.cache_generation(),
                    version,
                })
            }
            CacheLookup::Miss => {
                // registryを先に更新してからsubmitする (完了通知との競合を防ぐ)。
                // submit結果が拒否なら同じロック内で取り消す。
                let attempt = {
                    let mut registry = lock_registry(&self.registry);
                    let entry = registry.entry(key.hex.clone()).or_default();
                    entry.assets.push(asset.id);
                    entry.generation = self.cache_generation();
                    entry.version = version;
                    entry.size_class = size_class.ipc_name().to_string();
                    let job = Arc::new(ThumbnailJob {
                        source: asset.source_path.clone(),
                        fingerprint: fingerprint.clone(),
                        size_class,
                        cache: Arc::clone(&self.cache),
                        stats: Arc::clone(&self.stats),
                        #[cfg(any(debug_assertions, feature = "devtools"))]
                        injection: Arc::clone(&self.injection),
                    });
                    match self.pool.submit(&key.hex, priority, job) {
                        Ok(submitted) => Ok(submitted),
                        Err(err) => {
                            // 拒否: 台帳からこの要求を巻き戻す
                            let entry = registry.get_mut(&key.hex).expect("inserted above");
                            entry.assets.pop();
                            if entry.assets.is_empty() {
                                registry.remove(&key.hex);
                            }
                            Err(err)
                        }
                    }
                };
                match attempt {
                    Ok(submitted) => {
                        if submitted.is_merged() {
                            EnqueueOutcome::Merged
                        } else {
                            self.stats.record_miss();
                            EnqueueOutcome::Queued
                        }
                    }
                    Err(SubmitError::QueueFull) => EnqueueOutcome::Full,
                    Err(SubmitError::Shutdown) => EnqueueOutcome::Shutdown,
                }
            }
        }
    }
}

/// サムネイル生成サービス。AppStateへArcで保持する。
pub struct ThumbnailService {
    core: Arc<ThumbnailCore>,
    /// 全件生成の繰延残り (キュー空きを完了通知で待つ。WP-16 F1)。
    backlog: Arc<Mutex<BacklogState>>,
    /// `request_all` の世代トークン供給 (単調増加 — FG-WP16-3 B3)。
    request_generation: AtomicU64,
    /// feeder起床チャネルの複製 (backlog積み上げ直後の即時排液用)。
    feeder_wake: Sender<()>,
    feeder_stop: Arc<AtomicBool>,
    feeder: Mutex<Option<JoinHandle<()>>>,
    emitter_stop: Arc<AtomicBool>,
    emitter: Mutex<Option<JoinHandle<()>>>,
}

impl ThumbnailService {
    pub fn new(cache_root: PathBuf, events: Box<dyn ThumbEvents + Send>) -> io::Result<Arc<Self>> {
        Self::new_impl(
            cache_root,
            gento_image::cache::DEFAULT_MAX_BYTES,
            PoolConfig::default(),
            events,
        )
    }

    /// キャッシュ上限を指定して開く (WP-15: 設定 `cache.thumbs_max_bytes`
    /// の起動時適用。上限の実行時変更は `cache().set_max_bytes`)。
    pub fn new_with_limit(
        cache_root: PathBuf,
        max_bytes: u64,
        events: Box<dyn ThumbEvents + Send>,
    ) -> io::Result<Arc<Self>> {
        Self::new_impl(cache_root, max_bytes, PoolConfig::default(), events)
    }

    /// 共通構築。テストはプールキュー上限を縮小してbacklog経路を検証する。
    fn new_impl(
        cache_root: PathBuf,
        max_bytes: u64,
        pool_config: PoolConfig,
        events: Box<dyn ThumbEvents + Send>,
    ) -> io::Result<Arc<Self>> {
        let stats = Arc::new(ThumbStats::new());
        let cache = Arc::new(ThumbnailCache::open_with_limit(
            cache_root,
            max_bytes,
            Arc::clone(&stats),
        )?);
        let (pool, pool_rx) = WorkerPool::new(pool_config);
        let (emitter_tx, emitter_rx) = mpsc::channel();
        let (feeder_wake_tx, feeder_wake_rx) = mpsc::channel::<()>();
        let registry: Arc<Mutex<HashMap<String, PendingRequest>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let core = Arc::new(ThumbnailCore {
            cache: Arc::clone(&cache),
            pool: Arc::clone(&pool),
            stats: Arc::clone(&stats),
            registry: Arc::clone(&registry),
            cache_generation: AtomicU64::new(1),
            #[cfg(any(debug_assertions, feature = "devtools"))]
            injection: Arc::new(FailureInjection::new()),
            #[cfg(any(debug_assertions, feature = "devtools"))]
            feeder_gate: Arc::new(FeederGate::default()),
            emitter_tx: emitter_tx.clone(),
        });

        // プール完了チャネル → エミッタチャネルの橋渡し (単一チャネルでバッチングするため)。
        // 完了 = キュー空きでもあるため、backlog feederの起床信号を併送する。
        {
            let bridge_tx = emitter_tx.clone();
            let bridge_wake = feeder_wake_tx.clone();
            std::thread::spawn(move || {
                for completion in pool_rx {
                    let _ = bridge_tx.send(EmitterMsg::JobDone {
                        key: completion.key,
                        outcome: completion.outcome,
                    });
                    let _ = bridge_wake.send(());
                }
            });
        }

        let backlog = Arc::new(Mutex::new(BacklogState::default()));
        let feeder_stop = Arc::new(AtomicBool::new(false));
        let feeder = spawn_backlog_feeder(
            Arc::clone(&core),
            Arc::clone(&backlog),
            feeder_wake_rx,
            Arc::clone(&feeder_stop),
        );

        let emitter_stop = Arc::new(AtomicBool::new(false));
        let emitter = spawn_emitter(
            emitter_rx,
            events,
            registry,
            stats,
            cache,
            Arc::clone(&emitter_stop),
        );

        Ok(Arc::new(Self {
            core,
            backlog,
            request_generation: AtomicU64::new(0),
            feeder_wake: feeder_wake_tx,
            feeder_stop,
            feeder: Mutex::new(Some(feeder)),
            emitter_stop,
            emitter: Mutex::new(Some(emitter)),
        }))
    }

    pub fn cache_generation(&self) -> u64 {
        self.core.cache_generation()
    }

    /// プロトコルハンドラからのキャッシュ参照 (WP-4)。
    pub fn cache(&self) -> &Arc<ThumbnailCache> {
        &self.core.cache
    }

    /// backlog残件数 (テスト・診断用)。
    pub fn backlog_len(&self) -> usize {
        lock_backlog(&self.backlog).items.len()
    }

    /// 要求: ヒットは即時ready、ミスはジョブ投入 (dedup)。
    /// UI由来の要求は満杯を即時拒否する (backlogへ繰延しない — 表示要求は
    /// 時効性が高く、拒否を呼び出し側へ見せる方が正しい)。
    pub fn request(
        &self,
        assets: &[AssetSnapshot],
        size_class: SizeClass,
        priority: Priority,
    ) -> RequestAccepted {
        let mut counters = RequestAccepted::default();
        let mut immediate = Vec::new();
        for asset in assets {
            if !asset.readable {
                counters.skipped_unreadable += 1;
                continue;
            }
            match self.core.enqueue_asset(asset, size_class, priority) {
                EnqueueOutcome::Queued => counters.queued += 1,
                EnqueueOutcome::Merged => counters.rejected_dup += 1,
                EnqueueOutcome::CacheHit(item) => {
                    counters.cache_hits += 1;
                    immediate.push(item);
                }
                EnqueueOutcome::Full | EnqueueOutcome::Shutdown => {
                    counters.rejected_full += 1;
                }
            }
        }
        send_immediate(&self.core.emitter_tx, immediate);
        counters
    }

    /// 全件生成 (WP-16 F1): キュー上限で入れ切れなかった分は破棄せず
    /// backlogへ繰延し、プール完了駆動で空きができ次第投入する
    /// (library.rsのハッシュ再提出と同一の需要駆動方式)。返し値の
    /// `deferred` が繰延件数。既存のbacklogは新しいスナップショットで
    /// 置換され、排液完了・置換・終了時の残数はログで報告される。
    ///
    /// 世代安全性 (FG-WP16-3 B3 / FG-WP16-4 R2): 各呼び出しは単調増加の
    /// generation tokenを取得し、開始時にbacklogを自世代で初期化した上で、
    /// 繰延分を**単一ロック+世代検査で一括install**する。install時点でより
    /// 新しいrequest_allが開始済みなら自世代の繰延分は破棄される (backlogは
    /// 常に最新世代のみ)。feeder (drain_backlog) もpop済みitemの世代を
    /// プール投入**前**に同一ロック内で検査するため、新世代開始後に旧世代
    /// itemがプールへ投入される経路は存在しない。プールへ**既に投入済み**
    /// の旧世代jobは冪等なcache書き込みのため完了を許容する
    /// (仕様 — docs/dev/api-contract §8)。
    pub fn request_all(&self, assets: &[AssetSnapshot], size_class: SizeClass) -> RequestAccepted {
        let generation = self.request_generation.fetch_add(1, Ordering::SeqCst) + 1;
        {
            let mut backlog = lock_backlog(&self.backlog);
            if !backlog.items.is_empty() {
                tracing::info!(
                    replaced = backlog.items.len(),
                    deferred_total = backlog.deferred_total,
                    processed = backlog.processed,
                    "generate-all backlog replaced by a new request"
                );
            }
            backlog.items.clear();
            backlog.generation = generation;
            backlog.deferred_total = 0;
            backlog.processed = 0;
        }
        let mut counters = RequestAccepted::default();
        let mut immediate = Vec::new();
        let mut deferred: Vec<BacklogItem> = Vec::new();
        for asset in assets {
            if !asset.readable {
                counters.skipped_unreadable += 1;
                continue;
            }
            match self
                .core
                .enqueue_asset(asset, size_class, Priority::Background)
            {
                EnqueueOutcome::Queued => counters.queued += 1,
                EnqueueOutcome::Merged => counters.rejected_dup += 1,
                EnqueueOutcome::CacheHit(item) => {
                    counters.cache_hits += 1;
                    immediate.push(item);
                }
                EnqueueOutcome::Full => {
                    deferred.push(BacklogItem {
                        generation,
                        asset: asset.clone(),
                        size_class,
                        priority: Priority::Background,
                    });
                    counters.deferred += 1;
                }
                EnqueueOutcome::Shutdown => counters.rejected_full += 1,
            }
        }
        send_immediate(&self.core.emitter_tx, immediate);
        // 繰延分の一括install (世代検査付き — 旧実装の「clear後に逐次append」は
        // 並行request_allやdrainと交錯して混在backlogを生んだ)
        {
            let mut backlog = lock_backlog(&self.backlog);
            if backlog.generation == generation {
                backlog.deferred_total = deferred.len() as u64;
                backlog.items.extend(deferred);
            } else if !deferred.is_empty() {
                tracing::info!(
                    generation,
                    superseded_by = backlog.generation,
                    dropped = deferred.len(),
                    "generate-all deferred items discarded: superseded by a newer request"
                );
            }
        }
        // backlogを積んだ直後にすでに空きがある場合へ備えfeederを即時起床
        // させる (完了通知のみを待つと出発が遅れる)。
        let _ = self.feeder_wake.send(());
        counters
    }

    /// 未開始ジョブの優先度更新。
    pub fn reprioritize(
        &self,
        assets: &[AssetSnapshot],
        size_class: SizeClass,
        priority: Priority,
    ) -> ReprioritizeResult {
        let mut result = ReprioritizeResult::default();
        for asset in assets {
            if !asset.readable {
                result.skipped_unreadable += 1;
                continue;
            }
            let fingerprint =
                asset_fingerprint(&asset.source_path, asset.byte_size, asset.mtime_ms);
            let key = cache_key(&fingerprint, size_class);
            use gento_image::pool::ReprioritizeOutcome;
            match self.core.pool.reprioritize(&key.hex, priority) {
                ReprioritizeOutcome::Updated => result.updated += 1,
                ReprioritizeOutcome::Unchanged => result.not_updated += 1,
                ReprioritizeOutcome::NotFound => result.not_found += 1,
            }
        }
        result
    }

    /// 要求のキャンセル (未開始は即時、実行中は協調)。
    /// プールに**未投入**のbacklog分 (deque内の繰延entryとfeederがpop直後の
    /// inflight itemの両方) も同じキーで破棄する — 破棄分がfeederから
    /// 二度と投入されない (FG-WP16-3 B2 / FG-WP16-4 R1: pop→submit窓の
    /// 死角を閉じた)。
    pub fn cancel(&self, assets: &[AssetSnapshot], size_class: SizeClass) -> CancelResult {
        let mut result = CancelResult::default();
        // backlog (deque + inflight) から破棄したキー (プール未投入のため
        // pool.cancel_jobはNotFoundになる — not_foundに二重計上しない)
        let mut dropped_keys: HashSet<String> = HashSet::new();
        {
            let keys: HashSet<String> = assets
                .iter()
                .filter(|a| a.readable)
                .map(|a| {
                    cache_key(
                        &asset_fingerprint(&a.source_path, a.byte_size, a.mtime_ms),
                        size_class,
                    )
                    .hex
                })
                .collect();
            if !keys.is_empty() {
                let mut backlog = lock_backlog(&self.backlog);
                let mut dropped: u64 = 0;
                backlog.items.retain(|item| {
                    let key = cache_key(
                        &asset_fingerprint(
                            &item.asset.source_path,
                            item.asset.byte_size,
                            item.asset.mtime_ms,
                        ),
                        item.size_class,
                    );
                    if keys.contains(&key.hex) {
                        dropped += 1;
                        dropped_keys.insert(key.hex);
                        false
                    } else {
                        true
                    }
                });
                // feederがpop直後・まだプールへ投入していないitemも破棄
                // (inflightを差し替えるため、feederの投入判定はこのitemを
                // 廃棄する — 「not_found返却後の事後投入」は構造的に起きない)
                if let Some(inflight) = backlog.inflight.clone() {
                    if keys.contains(&inflight.key) {
                        backlog.inflight = None;
                        dropped += 1;
                        dropped_keys.insert(inflight.key);
                    }
                }
                if dropped > 0 {
                    tracing::info!(dropped, "generate-all backlog entries dropped by cancel");
                    result.cancelled_backlog = dropped;
                }
            }
        }
        for asset in assets {
            if !asset.readable {
                result.skipped_unreadable += 1;
                continue;
            }
            let fingerprint =
                asset_fingerprint(&asset.source_path, asset.byte_size, asset.mtime_ms);
            let key = cache_key(&fingerprint, size_class);
            use gento_image::pool::CancelOutcome;
            match self.core.pool.cancel_job(&key.hex) {
                CancelOutcome::CancelledQueued => result.cancelled_queued += 1,
                CancelOutcome::CancelRequested => result.cancel_requested += 1,
                CancelOutcome::NotFound => {
                    if !dropped_keys.contains(&key.hex) {
                        result.not_found += 1;
                    }
                }
            }
        }
        result
    }

    /// 統計スナップショット (command DTO)。プロトコル統計は
    /// `with_proto` でcommand層が上書きする (ここではゼロ初期化)。
    pub fn stats_dto(&self) -> ThumbnailStatsDto {
        let snapshot = self.core.stats.snapshot();
        let depths = self.core.pool.queue_depths_by_priority();
        ThumbnailStatsDto {
            generated: snapshot.generated,
            hits: snapshot.cache_hits,
            misses: snapshot.cache_misses,
            hit_rate: snapshot.hit_rate,
            failures: snapshot.failures,
            evictions: snapshot.evictions,
            queue_depth: self.core.pool.queue_depth(),
            queue_depths: QueueDepthsDto {
                interactive: depths[Priority::Interactive.as_u8() as usize],
                viewport: depths[Priority::Viewport.as_u8() as usize],
                prefetch: depths[Priority::Prefetch.as_u8() as usize],
                overscan: depths[Priority::Overscan.as_u8() as usize],
                background: depths[Priority::Background.as_u8() as usize],
            },
            cache_bytes: self.core.cache.total_bytes(),
            p50_ms: snapshot.p50_ms,
            p95_ms: snapshot.p95_ms,
            p99_ms: snapshot.p99_ms,
            paused: self.core.pool.is_paused(),
            #[cfg(any(debug_assertions, feature = "devtools"))]
            injection_mode: self.core.injection.mode().as_str().to_string(),
            #[cfg(any(debug_assertions, feature = "devtools"))]
            injection_rate: self.core.injection.rate(),
            proto_requests: 0,
            proto_ok: 0,
            proto_bytes_sent: 0,
            proto_p50_ms: None,
            proto_p95_ms: None,
            proto_p99_ms: None,
        }
    }

    /// 開発用エラー注入の設定 (WP-7 / フェーズ8)。即時に全ワーカーへ反映される。
    /// devtoolsビルドのみ (WP-0)。
    #[cfg(any(debug_assertions, feature = "devtools"))]
    pub fn set_injection(&self, mode: InjectionMode, rate: f64) {
        self.core.injection.set(mode, rate);
        tracing::info!(mode = mode.as_str(), rate, "failure injection updated");
    }

    /// テスト用: backlog feederの次のpop直後・投入直前で一時停止させる
    /// ゲートを装填する (一度だけ有効)。devtools/debug専用 (FG-WP16-4)。
    /// IPC非公開 — 決定論的raceテスト (単体テスト) 専用の注入。
    #[cfg(any(debug_assertions, feature = "devtools"))]
    pub fn debug_arm_feeder_gate(&self) -> FeederGateHandle {
        let (reached_tx, reached_rx) = mpsc::channel();
        let (go_tx, go_rx) = mpsc::channel();
        *self
            .core
            .feeder_gate
            .wiring
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(FeederGateWiring {
            reached: reached_tx,
            go: go_rx,
        });
        FeederGateHandle {
            reached: reached_rx,
            go: go_tx,
        }
    }

    pub fn set_paused(&self, paused: bool) {
        self.core.pool.set_paused(paused);
    }

    /// 待機中ジョブを全キャンセルし、キャッシュを全削除してgenerationを進める。
    /// 実行中ジョブの書き込みはclear後にコミットされる場合がある
    /// (内容はキーに対応する正当なサムネイルのため無害)。
    pub fn clear_cache(&self) -> io::Result<ClearCacheResult> {
        // backlogも全件生成の継続意図ごと破棄する (削除直後に再生成が
        // 再開されるのを防ぐ — キャッシュ全削除は明示的な中止指示)。
        let dropped = {
            let mut backlog = lock_backlog(&self.backlog);
            let dropped = backlog.items.len();
            *backlog = BacklogState::default();
            dropped
        };
        if dropped > 0 {
            tracing::info!(dropped, "generate-all backlog dropped by cache clear");
        }
        // 台帳に残るキー = 未完了ジョブ。全てキャンセルする。
        let keys: Vec<String> = lock_registry(&self.core.registry).keys().cloned().collect();
        for key in keys {
            self.core.pool.cancel_job(&key);
        }
        let outcome = self.core.cache.clear()?;
        let generation = self.core.cache_generation.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(ClearCacheResult {
            removed_files: outcome.removed_files,
            removed_bytes: outcome.removed_bytes,
            cache_generation: generation,
        })
    }

    /// backlog feeder・プール・エミッタを停止する (アプリ終了時。UIスレッドから呼ぶ)。
    pub fn shutdown(&self) {
        // feederを先に停止する (プール停止後のsubmitでアイテムが滞留しない。
        // 残りはfeeder側でログ報告される)。
        self.feeder_stop.store(true, Ordering::SeqCst);
        let _ = self.feeder_wake.send(());
        if let Some(handle) = self.feeder.lock().unwrap_or_else(|p| p.into_inner()).take() {
            let _ = handle.join();
        }
        self.core.pool.shutdown();
        self.emitter_stop.store(true, Ordering::SeqCst);
        // エミッタのrecvを即時起床させるためダミーメッセージを送る
        let _ = self
            .core
            .emitter_tx
            .send(EmitterMsg::ImmediateReady { items: Vec::new() });
        if let Some(handle) = self
            .emitter
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            let _ = handle.join();
        }
    }
}

fn lock_registry(
    registry: &Mutex<HashMap<String, PendingRequest>>,
) -> std::sync::MutexGuard<'_, HashMap<String, PendingRequest>> {
    registry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn lock_backlog(backlog: &Mutex<BacklogState>) -> std::sync::MutexGuard<'_, BacklogState> {
    backlog
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ===== テスト用feederゲート (devtools/debug専用 — FG-WP16-4) =====

/// backlog feederのpop直後・投入直前に一時停止する注入フック
/// (決定論的インターリーブテスト用。`set_failure_injection` と同じ
/// コンパイルゲートで、prod releaseには存在しない)。backlogロックは
/// 保持したまま待機しない — 停止中もcancel/request_allは進行でき、
/// それらとfeeder投入の競合窓を決定論的に再現する。
#[cfg(any(debug_assertions, feature = "devtools"))]
#[derive(Debug, Default)]
pub struct FeederGate {
    /// 一度だけ有効な接続 (装填→feeder到達→解放の1往復)。
    wiring: Mutex<Option<FeederGateWiring>>,
}

#[cfg(any(debug_assertions, feature = "devtools"))]
#[derive(Debug)]
struct FeederGateWiring {
    reached: Sender<()>,
    go: Receiver<()>,
}

#[cfg(any(debug_assertions, feature = "devtools"))]
impl FeederGate {
    /// feeder側から呼ぶ: 接続されていれば到達を通知して解放を待つ。
    /// 接続がない (通常運用) 場合は即座に通過する。
    fn wait_if_armed(&self) {
        let wiring = self
            .wiring
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(wiring) = wiring {
            let _ = wiring.reached.send(());
            // 解放を待つ。ハンドルdropも解放扱い (試験停滞防止)。
            let _ = wiring.go.recv();
        }
    }
}

/// [`ThumbnailService::debug_arm_feeder_gate`] が返すテスト側ハンドル。
#[cfg(any(debug_assertions, feature = "devtools"))]
pub struct FeederGateHandle {
    reached: Receiver<()>,
    go: Sender<()>,
}

#[cfg(any(debug_assertions, feature = "devtools"))]
impl FeederGateHandle {
    /// feederがゲートへ到達した (backlog先頭をpopした) ことを待つ。
    pub fn wait_reached(&self) -> bool {
        self.reached.recv_timeout(Duration::from_secs(10)).is_ok()
    }

    /// ゲートを解放し、feederの投入判定を再開させる。
    pub fn release(&self) {
        let _ = self.go.send(());
    }
}

/// 即時readyアイテムをエミッタへ送る (投入経路の共通末尾)。
fn send_immediate(emitter_tx: &Sender<EmitterMsg>, items: Vec<ThumbReadyItem>) {
    if !items.is_empty() {
        let _ = emitter_tx.send(EmitterMsg::ImmediateReady { items });
    }
}

/// backlog feeder: プール完了通知 (または紛失通知保険の200ms) で起床し、
/// キューの空きぶんだけbacklogから再投入する。満杯に当たったアイテムは
/// 先頭へ戻して次の完了で再試行する (ハッシュパイプラインの再提出と同一)。
fn spawn_backlog_feeder(
    core: Arc<ThumbnailCore>,
    backlog: Arc<Mutex<BacklogState>>,
    wake: Receiver<()>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("thumb-backlog-feeder".into())
        .spawn(move || loop {
            if stop.load(Ordering::SeqCst) {
                let remaining = lock_backlog(&backlog).items.len();
                if remaining > 0 {
                    tracing::info!(
                        remaining,
                        "thumbnail generate-all backlog dropped at shutdown"
                    );
                }
                return;
            }
            match wake.recv_timeout(Duration::from_millis(200)) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
            drain_backlog(&core, &backlog, &stop);
        })
        .expect("failed to spawn thumb backlog feeder")
}

/// backlogをキュー空きぶんだけ投入する (1起床で空きが尽きるまで繰り返す)。
///
/// FG-WP16-4 R1/R2: 各itemの投入はbacklogロックで守られた2区間で行う。
/// ① pop区間: itemをdequeからinflightへ移す (この直後からcancelに可視)。
/// ② 投入区間: cancelがinflightを破棄していないか (pop_id一致) と世代が
/// 現行かを検査してから `enqueue_asset` によるプール投入 (満杯時の先頭戻し
/// を含む) までを**同一ロック内で完結**させる。cancelも同じロック内で
/// (a) deque内の一致key破棄 (b) inflightの一致key破棄を行うため、
/// 「cancelがnot_foundで完了した後にそのキーがfeeder経由で投入される」
/// 経路と「より新しいrequest_all開始後の旧世代item投入」は構造的に
/// 存在しない。投入区間内の `enqueue_asset` はregistryロック・pool
/// キューロック取得と非ブロッキングchannel送信のみで、backlogロック
/// 保持中に呼んでも安全 (ロック順序は常に backlog → registry → pool)。
fn drain_backlog(core: &ThumbnailCore, backlog: &Mutex<BacklogState>, stop: &AtomicBool) {
    let capacity = core.pool.queue_capacity();
    let margin = BACKLOG_SUBMIT_MARGIN.min(capacity / 4);
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let free = capacity
            .saturating_sub(core.pool.queue_depth())
            .saturating_sub(margin);
        if free == 0 {
            return;
        }
        let mut taken = 0usize;
        let mut immediate = Vec::new();
        let mut hit_full = false;
        while taken < free.min(BACKLOG_SUBMIT_BATCH) {
            let Some((pop_id, item)) = lock_backlog(backlog).pop_for_feeder() else {
                break;
            };
            taken += 1;
            // テスト注入ゲート: pop後・投入判定前に一時停止 (ロック保持なし
            // — 停止中のcancel/request_allの進行を妨げない)
            #[cfg(any(debug_assertions, feature = "devtools"))]
            core.feeder_gate.wait_if_armed();
            // 投入区間: cancel消費・世代検査・プール投入を同一ロックで。
            // 破棄判定に当たった場合はcontinueで次のitemへ (cancel側で
            // 計上済みのためここでは何も報告しない)。
            let step = {
                let mut b = lock_backlog(backlog);
                let still_current = matches!(&b.inflight, Some(inf) if inf.pop_id == pop_id);
                if !still_current {
                    // cancelがinflightを破棄済み、またはclear/置換で状態が
                    // 初期化済み — このitemは投入しない。
                    continue;
                }
                if b.generation != item.generation {
                    // より新しいrequest_all (またはclear) が開始済み —
                    // 旧世代itemは投入しない (FG-WP16-4 R2)。
                    b.inflight = None;
                    tracing::debug!(
                        generation = item.generation,
                        superseded_by = b.generation,
                        "popped backlog item discarded: superseded before enqueue"
                    );
                    continue;
                }
                b.inflight = None;
                match core.enqueue_asset(&item.asset, item.size_class, item.priority) {
                    EnqueueOutcome::Queued | EnqueueOutcome::Merged => {
                        b.processed += 1;
                        FeedStep::Submitted
                    }
                    EnqueueOutcome::CacheHit(hit) => {
                        b.processed += 1;
                        FeedStep::Immediate(hit)
                    }
                    // 他の投入と競合して満杯: 先頭へ戻して次の完了で再試行
                    // (自世代のみ — 世代は上で検査済み)
                    EnqueueOutcome::Full => {
                        b.items.push_front(item);
                        FeedStep::RequeuedFull
                    }
                    // プール停止: 以降の再投入は無意味 (残りは停止時報告)
                    EnqueueOutcome::Shutdown => {
                        b.items.push_front(item);
                        FeedStep::RequeuedShutdown
                    }
                }
            };
            match step {
                FeedStep::Submitted => {}
                FeedStep::Immediate(hit) => immediate.push(hit),
                FeedStep::RequeuedFull => {
                    hit_full = true;
                    break;
                }
                FeedStep::RequeuedShutdown => {
                    send_immediate(&core.emitter_tx, immediate);
                    return;
                }
            }
        }
        send_immediate(&core.emitter_tx, immediate);
        {
            let mut b = lock_backlog(backlog);
            if b.items.is_empty() && (b.deferred_total > 0 || b.processed > 0) {
                tracing::info!(
                    deferred = b.deferred_total,
                    processed = b.processed,
                    "generate-all backlog drained"
                );
                b.deferred_total = 0;
                b.processed = 0;
            }
        }
        if hit_full || taken == 0 {
            return;
        }
    }
}

/// drain_backlogの1item分の投入区間の結果。
enum FeedStep {
    /// 投入または既存ジョブへ統合 (processed更新済み)。
    Submitted,
    /// キャッシュヒット (processed更新済み。呼び出し側で即時readyへ集約)。
    Immediate(ThumbReadyItem),
    /// キュー満杯で先頭へ戻した。
    RequeuedFull,
    /// プール停止で先頭へ戻した。
    RequeuedShutdown,
}

/// fingerprintからURLバージョンを計算する (キャッシュキー計算と同一入力から
/// 導くため、キーが変われば必ずバージョンも変わる)。
fn content_version_of(fingerprint: &gento_image::cache::AssetFingerprint) -> u64 {
    gento_image::cache::content_version(fingerprint)
}

/// バッチング付きイベント送出スレッド。
fn spawn_emitter(
    rx: Receiver<EmitterMsg>,
    events: Box<dyn ThumbEvents + Send>,
    registry: Arc<Mutex<HashMap<String, PendingRequest>>>,
    stats: Arc<ThumbStats>,
    cache: Arc<ThumbnailCache>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("thumb-emitter".into())
        .spawn(move || {
            let mut emitted: u64 = 0;
            'outer: while !stop.load(Ordering::SeqCst) {
                let first = match rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(msg) => msg,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break 'outer,
                };
                let mut batch: Vec<EmitterMsg> = vec![first];
                let deadline = Instant::now() + EVENT_BATCH_WINDOW;
                while batch.len() < EVENT_BATCH_MAX {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        break;
                    }
                    match rx.recv_timeout(remaining) {
                        Ok(msg) => batch.push(msg),
                        Err(mpsc::RecvTimeoutError::Timeout) => break,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }

                let mut ready: Vec<ThumbReadyItem> = Vec::new();
                let mut failed: Vec<ThumbFailedItem> = Vec::new();
                for msg in batch {
                    match msg {
                        EmitterMsg::JobDone { key, outcome } => {
                            let pending = lock_registry(&registry).remove(&key);
                            match (outcome, pending) {
                                (OutcomeSummary::Completed, Some(p)) => {
                                    for asset_id in p.assets {
                                        ready.push(ThumbReadyItem {
                                            asset_id,
                                            size_class: p.size_class.clone(),
                                            cache_generation: p.generation,
                                            version: p.version,
                                        });
                                    }
                                }
                                (OutcomeSummary::Failed(reason), Some(p)) => {
                                    stats.record_failure();
                                    for asset_id in p.assets {
                                        failed.push(ThumbFailedItem {
                                            asset_id,
                                            size_class: p.size_class.clone(),
                                            reason: reason.clone(),
                                        });
                                    }
                                }
                                _ => {
                                    // Cancelled または台帳になし (クリア済み等): 何も流さない
                                }
                            }
                        }
                        EmitterMsg::ImmediateReady { items } => ready.extend(items),
                    }
                }
                // 1メッセージが多数アイテムを運ぶことがあるため、
                // 送出時にアイテム数でも上限を強制する
                for chunk in ready.chunks(EVENT_BATCH_MAX) {
                    events.ready(chunk);
                    emitted += chunk.len() as u64;
                }
                for chunk in failed.chunks(EVENT_BATCH_MAX) {
                    events.failed(chunk);
                }
                if emitted >= EMITTER_SUMMARY_EVERY {
                    let snapshot = stats.snapshot();
                    tracing::info!(
                        generated = snapshot.generated,
                        hits = snapshot.cache_hits,
                        misses = snapshot.cache_misses,
                        failures = snapshot.failures,
                        evictions = snapshot.evictions,
                        cache_bytes = cache.total_bytes(),
                        "thumb metrics (periodic)"
                    );
                    emitted = 0;
                }
            }
            tracing::debug!("thumb emitter stopped");
        })
        .expect("failed to spawn thumb emitter")
}

/// `reprioritize_thumbnails` の結果DTO。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
pub struct ReprioritizeResult {
    pub updated: u64,
    /// 実行中・同一優先度のため変更なし。
    pub not_updated: u64,
    pub not_found: u64,
    pub skipped_unreadable: u64,
}

/// `cancel_thumbnail_requests` の結果DTO。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
pub struct CancelResult {
    pub cancelled_queued: u64,
    pub cancel_requested: u64,
    /// backlog (プール未投入のgenerate-all繰延分 — deque内のentryと
    /// feederがpop直後のinflight itemの両方) から破棄した件数
    /// (FG-WP16-3 B2 / FG-WP16-4 R1 — 破棄分は差し替え投入されない)。
    pub cancelled_backlog: u64,
    pub not_found: u64,
    pub skipped_unreadable: u64,
}

/// `get_thumbnail_stats` の結果DTO。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ThumbnailStatsDto {
    pub generated: u64,
    pub hits: u64,
    pub misses: u64,
    pub hit_rate: Option<f64>,
    pub failures: u64,
    pub evictions: u64,
    pub queue_depth: usize,
    /// 優先度別の未開始ジョブ数 (WP-7開発パネル用)。
    pub queue_depths: QueueDepthsDto,
    pub cache_bytes: u64,
    pub p50_ms: Option<f64>,
    pub p95_ms: Option<f64>,
    pub p99_ms: Option<f64>,
    pub paused: bool,
    /// 開発用エラー注入の現在値 (WP-7)。devtoolsビルドのみ (WP-0)。
    #[cfg(any(debug_assertions, feature = "devtools"))]
    pub injection_mode: String,
    #[cfg(any(debug_assertions, feature = "devtools"))]
    pub injection_rate: f64,
    // ローカル画像プロトコル統計 (WP-4。get_thumbnail_statsへ統合)
    pub proto_requests: u64,
    pub proto_ok: u64,
    pub proto_bytes_sent: u64,
    pub proto_p50_ms: Option<f64>,
    pub proto_p95_ms: Option<f64>,
    pub proto_p99_ms: Option<f64>,
}

/// 優先度別キュー件数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
pub struct QueueDepthsDto {
    pub interactive: usize,
    pub viewport: usize,
    pub prefetch: usize,
    pub overscan: usize,
    pub background: usize,
}

/// `set_failure_injection` の結果DTO (現在の設定を返す)。
/// devtoolsビルドのみ (WP-0)。
#[cfg(any(debug_assertions, feature = "devtools"))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InjectionStatusDto {
    pub mode: String,
    pub rate: f64,
}

impl ThumbnailStatsDto {
    /// プロトコル統計を反映する (command層でAppStateから渡す)。
    pub fn with_proto(mut self, proto: &crate::protocol::ProtoStatsSnapshot) -> Self {
        self.proto_requests = proto.requests;
        self.proto_ok = proto.ok;
        self.proto_bytes_sent = proto.bytes_sent;
        self.proto_p50_ms = proto.p50_ms;
        self.proto_p95_ms = proto.p95_ms;
        self.proto_p99_ms = proto.p99_ms;
        self
    }
}

/// `clear_thumbnail_cache` の結果DTO。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ClearCacheResult {
    pub removed_files: u64,
    pub removed_bytes: u64,
    pub cache_generation: u64,
}

/// Tauriイベントへの送出実装。
pub struct TauriThumbEvents {
    pub app: tauri::AppHandle,
}

impl ThumbEvents for TauriThumbEvents {
    fn ready(&self, items: &[ThumbReadyItem]) {
        #[derive(Clone, Serialize)]
        struct Payload<'a> {
            items: &'a [ThumbReadyItem],
        }
        let _ = self.app.emit("thumb:ready", Payload { items });
    }

    fn failed(&self, items: &[ThumbFailedItem]) {
        #[derive(Clone, Serialize)]
        struct Payload<'a> {
            items: &'a [ThumbFailedItem],
        }
        let _ = self.app.emit("thumb:failed", Payload { items });
    }
}

/// カタログからコマンド引数のID群を解決する。未登録IDはskippedに数える。
fn resolve_assets(state: &AppState, ids: &[AssetId]) -> (Vec<AssetSnapshot>, u64) {
    let inner = state.lock_catalog();
    let mut assets = Vec::with_capacity(ids.len());
    let mut unknown = 0;
    for id in ids {
        match inner.record(*id) {
            Some(record) => assets.push(AssetSnapshot {
                id: record.id,
                source_path: record.source_path.clone(),
                byte_size: record.byte_size,
                mtime_ms: record.mtime_ms,
                readable: record.state == gento_core::catalog::ImageState::Ready,
                content_type: record
                    .mime_type
                    .clone()
                    .unwrap_or_else(|| "application/octet-stream".to_string()),
            }),
            None => unknown += 1,
        }
    }
    (assets, unknown)
}

/// 表示中カタログ全体のスナップショット (generate_all用)。
fn snapshot_all(state: &AppState) -> Vec<AssetSnapshot> {
    let inner = state.lock_catalog();
    inner
        .records()
        .iter()
        .map(|record| AssetSnapshot {
            id: record.id,
            source_path: record.source_path.clone(),
            byte_size: record.byte_size,
            mtime_ms: record.mtime_ms,
            readable: record.state == gento_core::catalog::ImageState::Ready,
            content_type: record
                .mime_type
                .clone()
                .unwrap_or_else(|| "application/octet-stream".to_string()),
        })
        .collect()
}

fn parse_ids(ids: &[String]) -> Result<Vec<AssetId>, String> {
    ids.iter()
        .map(|s| {
            s.parse::<AssetId>()
                .map_err(|e| format!("invalid asset id {s:?}: {e}"))
        })
        .collect()
}

#[tauri::command]
pub fn request_thumbnails(
    state: State<'_, AppState>,
    ids: Vec<String>,
    size_class: String,
    priority: String,
) -> Result<RequestAccepted, String> {
    let size_class: SizeClass = size_class
        .parse()
        .map_err(|e: gento_core::ParseSizeClassError| e.to_string())?;
    let priority: Priority = priority
        .parse()
        .map_err(|e: gento_core::ParsePriorityError| e.to_string())?;
    let ids = parse_ids(&ids)?;
    let (assets, unknown) = resolve_assets(&state, &ids);
    let mut accepted = state.thumbnails().request(&assets, size_class, priority);
    accepted.skipped_unknown = unknown;
    Ok(accepted)
}

#[tauri::command]
pub fn reprioritize_thumbnails(
    state: State<'_, AppState>,
    ids: Vec<String>,
    size_class: String,
    priority: String,
) -> Result<ReprioritizeResult, String> {
    let size_class: SizeClass = size_class
        .parse()
        .map_err(|e: gento_core::ParseSizeClassError| e.to_string())?;
    let priority: Priority = priority
        .parse()
        .map_err(|e: gento_core::ParsePriorityError| e.to_string())?;
    let ids = parse_ids(&ids)?;
    let (assets, unknown) = resolve_assets(&state, &ids);
    let mut result = state
        .thumbnails()
        .reprioritize(&assets, size_class, priority);
    result.skipped_unreadable += unknown;
    Ok(result)
}

#[tauri::command]
pub fn cancel_thumbnail_requests(
    state: State<'_, AppState>,
    ids: Vec<String>,
    size_class: String,
) -> Result<CancelResult, String> {
    let size_class: SizeClass = size_class
        .parse()
        .map_err(|e: gento_core::ParseSizeClassError| e.to_string())?;
    let ids = parse_ids(&ids)?;
    let (assets, unknown) = resolve_assets(&state, &ids);
    let mut result = state.thumbnails().cancel(&assets, size_class);
    result.skipped_unreadable += unknown;
    Ok(result)
}

#[tauri::command]
pub fn generate_all_thumbnails(
    state: State<'_, AppState>,
    size_class: String,
) -> Result<RequestAccepted, String> {
    let size_class: SizeClass = size_class
        .parse()
        .map_err(|e: gento_core::ParseSizeClassError| e.to_string())?;
    let assets = snapshot_all(&state);
    // キュー上限を超えた分はbacklogへ繰延され完了駆動で投入される
    // (WP-16 F1。かつての黙示打切りは廃止)。
    Ok(state.thumbnails().request_all(&assets, size_class))
}

#[tauri::command]
pub fn get_thumbnail_stats(state: State<'_, AppState>) -> ThumbnailStatsDto {
    state
        .thumbnails()
        .stats_dto()
        .with_proto(&state.proto_stats.snapshot())
}

/// 開発用エラー注入 (WP-7 / フェーズ8)。mode = none|decode|cache_write、
/// rate = 0.0..1.0。決定的 (キャッシュキー閾値) ため計測が再現可能。
/// devtoolsビルドのみにコンパイルされる (WP-0)。ビルド内では計測ブリッジ
/// (`GENTO_DEV_BRIDGE=1`) が無効な場合Err (FG-1)。
#[cfg(any(debug_assertions, feature = "devtools"))]
#[tauri::command]
pub fn set_failure_injection(
    state: State<'_, AppState>,
    mode: String,
    rate: f64,
) -> Result<InjectionStatusDto, String> {
    set_failure_injection_impl(&state, &mode, rate)
}

/// [`set_failure_injection`] のゲート付き本体 (単体テスト用にStateから切り出した)。
#[cfg(any(debug_assertions, feature = "devtools"))]
pub(crate) fn set_failure_injection_impl(
    state: &AppState,
    mode: &str,
    rate: f64,
) -> Result<InjectionStatusDto, String> {
    crate::devtools::dev_bridge_gate(state)?;
    if !(0.0..=1.0).contains(&rate) {
        return Err(format!("rate must be within 0.0..=1.0 (got {rate})"));
    }
    let mode = InjectionMode::from_ipc(mode)
        .ok_or_else(|| format!("unknown injection mode {mode:?}: none|decode|cache_write"))?;
    state.thumbnails().set_injection(mode, rate);
    Ok(InjectionStatusDto {
        mode: mode.as_str().to_string(),
        rate,
    })
}

#[tauri::command]
pub fn pause_thumbnail_generation(state: State<'_, AppState>, paused: bool) {
    state.thumbnails().set_paused(paused);
}

#[tauri::command]
pub fn clear_thumbnail_cache(state: State<'_, AppState>) -> Result<ClearCacheResult, String> {
    state.thumbnails().clear_cache().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::sync::Mutex as StdMutex;

    static DIR_SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let seq = DIR_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "gento-tauri-thumb-test-{}-{}-{}",
                name,
                std::process::id(),
                seq
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        /// 実PNGを書く (image crate。テスト専用dev-dependency)。
        fn write_png(&self, name: &str, w: u32, h: u32) -> PathBuf {
            let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(w, h, |x, y| {
                image::Rgba([x as u8 % 251, y as u8 % 251, 128, 255])
            }));
            let p = self.0.join(name);
            img.save(&p).unwrap();
            p
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Debug, Clone)]
    enum RecordedBatch {
        Ready(Vec<ThumbReadyItem>),
        Failed(Vec<ThumbFailedItem>),
    }

    /// テスト用の記録イベント実装。Arc共有でテスト側からも読めるようにする。
    #[derive(Clone, Default)]
    struct SharedEvents(Arc<StdMutex<Vec<RecordedBatch>>>);

    impl SharedEvents {
        fn batches(&self) -> Vec<RecordedBatch> {
            self.0.lock().unwrap().clone()
        }

        fn ready_items(&self) -> Vec<ThumbReadyItem> {
            self.batches()
                .into_iter()
                .flat_map(|b| match b {
                    RecordedBatch::Ready(items) => items,
                    RecordedBatch::Failed(_) => Vec::new(),
                })
                .collect()
        }

        fn failed_items(&self) -> Vec<ThumbFailedItem> {
            self.batches()
                .into_iter()
                .flat_map(|b| match b {
                    RecordedBatch::Failed(items) => items,
                    RecordedBatch::Ready(_) => Vec::new(),
                })
                .collect()
        }

        fn clear(&self) {
            self.0.lock().unwrap().clear();
        }

        fn wait_ready_count(&self, count: usize) -> Vec<ThumbReadyItem> {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let items = self.ready_items();
                if items.len() >= count {
                    return items;
                }
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for {count} ready items"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    impl ThumbEvents for SharedEvents {
        fn ready(&self, items: &[ThumbReadyItem]) {
            if !items.is_empty() {
                self.0
                    .lock()
                    .unwrap()
                    .push(RecordedBatch::Ready(items.to_vec()));
            }
        }

        fn failed(&self, items: &[ThumbFailedItem]) {
            if !items.is_empty() {
                self.0
                    .lock()
                    .unwrap()
                    .push(RecordedBatch::Failed(items.to_vec()));
            }
        }
    }

    fn asset(id: u64, path: &Path, readable: bool) -> AssetSnapshot {
        let (byte_size, mtime_ms) = match fs::metadata(path) {
            Ok(md) => (
                md.len(),
                md.modified()
                    .unwrap()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64,
            ),
            Err(_) => (0, 0),
        };
        AssetSnapshot {
            id: AssetId::new(id),
            source_path: path.to_path_buf(),
            byte_size,
            mtime_ms,
            readable,
            content_type: "image/png".to_string(),
        }
    }

    fn service(dir: &TestDir, events: SharedEvents) -> Arc<ThumbnailService> {
        ThumbnailService::new(dir.path().join("cache"), Box::new(events)).unwrap()
    }

    /// プールキュー上限を縮小したサービス (WP-16 F1のbacklog経路検証用)。
    fn service_with_queue_capacity(
        dir: &TestDir,
        events: SharedEvents,
        queue_capacity: usize,
    ) -> Arc<ThumbnailService> {
        let pool_config = PoolConfig {
            worker_count: 2,
            queue_capacity,
        };
        ThumbnailService::new_impl(
            dir.path().join("cache"),
            gento_image::cache::DEFAULT_MAX_BYTES,
            pool_config,
            Box::new(events),
        )
        .unwrap()
    }

    /// backlogが空になるまで待つ (feederの排液は完了通知駆動のため非同期)。
    fn wait_backlog_drained(svc: &ThumbnailService) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while svc.backlog_len() > 0 {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for backlog to drain (remaining={})",
                svc.backlog_len()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    // ===== WP-16 F1: 全件生成のキュー上限繰延 (backlog) =====

    /// 1回のrequest_allで上限超過分も含め全件が生成される (黙示打切りの回帰)。
    #[test]
    fn generate_all_defers_overflow_and_drains_all_beyond_queue_capacity() {
        let dir = TestDir::new("backlog-drain");
        let events = SharedEvents::default();
        let svc = service_with_queue_capacity(&dir, events.clone(), 4);
        svc.set_paused(true); // 決定的にキューを埋める (ワーカーに消費させない)
        let assets: Vec<AssetSnapshot> = (0..12)
            .map(|i| {
                asset(
                    u64::try_from(i).unwrap() + 1,
                    &dir.write_png(&format!("g{i:02}.png"), 64, 64),
                    true,
                )
            })
            .collect();

        let accepted = svc.request_all(&assets, SizeClass::S256);
        assert_eq!(accepted.queued, 4, "capacity=4に達するまで投入");
        assert_eq!(accepted.deferred, 8, "残りは黙示破棄ではなく繰延");
        assert_eq!(accepted.rejected_full, 0);
        assert_eq!(svc.backlog_len(), 8);

        svc.set_paused(false);
        // 再投入なしの1回のrequest_allで全12件が生成される
        let ready = events.wait_ready_count(12);
        assert_eq!(ready.len(), 12);
        wait_backlog_drained(&svc);
        let dto = svc.stats_dto();
        assert_eq!(dto.generated, 12);
        assert_eq!(dto.queue_depth, 0);
        svc.shutdown();
    }

    /// UI由来了のrequestは満杯をbacklogにせず即時拒否する (需要駆動ensure整合)。
    #[test]
    fn request_rejects_full_without_backlog() {
        let dir = TestDir::new("backlog-none");
        let events = SharedEvents::default();
        let svc = service_with_queue_capacity(&dir, events.clone(), 2);
        svc.set_paused(true);
        let assets: Vec<AssetSnapshot> = (0..3)
            .map(|i| {
                asset(
                    u64::try_from(i).unwrap() + 1,
                    &dir.write_png(&format!("r{i}.png"), 64, 64),
                    true,
                )
            })
            .collect();
        let counters = svc.request(&assets, SizeClass::S256, Priority::Viewport);
        assert_eq!(counters.queued, 2);
        assert_eq!(counters.rejected_full, 1);
        assert_eq!(counters.deferred, 0);
        assert_eq!(svc.backlog_len(), 0, "requestはbacklogへ積まない");
        svc.shutdown();
    }

    /// キャッシュ全削除はbacklogも破棄し、shutdownは残りを残したまま完了する。
    #[test]
    fn generate_all_backlog_dropped_on_clear_cache_and_reports_at_shutdown() {
        let dir = TestDir::new("backlog-drop");
        let events = SharedEvents::default();
        let svc = service_with_queue_capacity(&dir, events.clone(), 2);
        svc.set_paused(true);
        let assets: Vec<AssetSnapshot> = (0..6)
            .map(|i| {
                asset(
                    u64::try_from(i).unwrap() + 1,
                    &dir.write_png(&format!("d{i}.png"), 64, 64),
                    true,
                )
            })
            .collect();
        let accepted = svc.request_all(&assets, SizeClass::S256);
        assert_eq!(accepted.queued, 2);
        assert_eq!(accepted.deferred, 4);

        // clear_cache: 未完了ジョブのキャンセル + backlog破棄
        svc.clear_cache().unwrap();
        assert_eq!(svc.backlog_len(), 0, "キャッシュ全削除でbacklogも破棄");
        svc.set_paused(false);
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            events.ready_items().is_empty(),
            "キャンセル済みジョブ・破棄backlogからは生成されない"
        );

        // 再度繰延を作った状態でshutdown: 残り報告の上で停止完了する
        svc.set_paused(true); // 決定的に繰延を作る (ワーカーに消費させない)
        let accepted2 = svc.request_all(&assets, SizeClass::S256);
        assert_eq!(accepted2.deferred, 4);
        svc.shutdown();
        assert_eq!(
            svc.backlog_len(),
            4,
            "shutdown時点の残りは報告 (ログ) のうえ投入されずに残る"
        );
    }

    /// FG-WP16-3 B2: cancelはプール未投入のbacklog entriesも破棄する
    /// (feederが破棄分を二度と投入しない)。
    #[test]
    fn cancel_discards_deferred_backlog_entries() {
        let dir = TestDir::new("backlog-cancel");
        let events = SharedEvents::default();
        let svc = service_with_queue_capacity(&dir, events.clone(), 1);
        svc.set_paused(true); // 決定的にキューを埋める
        let assets: Vec<AssetSnapshot> = (0..3)
            .map(|i| {
                asset(
                    u64::try_from(i).unwrap() + 1,
                    &dir.write_png(&format!("c{i}.png"), 64, 64),
                    true,
                )
            })
            .collect();
        let accepted = svc.request_all(&assets, SizeClass::S256);
        assert_eq!(accepted.queued, 1);
        assert_eq!(accepted.deferred, 2);
        assert_eq!(svc.backlog_len(), 2);

        let cancel = svc.cancel(&assets, SizeClass::S256);
        assert_eq!(cancel.cancelled_queued, 1, "{cancel:?}");
        assert_eq!(
            cancel.cancelled_backlog, 2,
            "backlog分も破棄報告: {cancel:?}"
        );
        assert_eq!(cancel.not_found, 0, "{cancel:?}");
        assert_eq!(svc.backlog_len(), 0, "cancelでbacklogは空になる");

        // 解除して十分待っても、破棄分が投入されることはない
        svc.set_paused(false);
        std::thread::sleep(Duration::from_millis(500));
        assert!(
            events.ready_items().is_empty(),
            "キャンセル済み資産は生成されない: {:?}",
            events.ready_items()
        );
        assert_eq!(svc.stats_dto().generated, 0);
        svc.shutdown();
    }

    /// FG-WP16-3 B3: 再押下 (request_all) はbacklogを最新世代で置換する。
    /// プールに投入済みの旧世代jobの完了は冪等なcache書き込みのため許容、
    /// backlogから投入されるのは最後のrequestの資産のみ。
    #[test]
    fn generate_all_repress_replaces_backlog_with_latest_generation() {
        let dir = TestDir::new("backlog-repress");
        let events = SharedEvents::default();
        let svc = service_with_queue_capacity(&dir, events.clone(), 2);
        svc.set_paused(true);
        let mk = |prefix: &str, base_id: u64, n: usize| -> Vec<AssetSnapshot> {
            (0..n)
                .map(|i| {
                    asset(
                        base_id + u64::try_from(i).unwrap(),
                        &dir.write_png(&format!("{prefix}{i}.png"), 64, 64),
                        true,
                    )
                })
                .collect()
        };
        let a = mk("a", 1, 4);
        let b = mk("b", 100, 2);

        let first = svc.request_all(&a, SizeClass::S256);
        assert_eq!(first.queued, 2);
        assert_eq!(first.deferred, 2, "a2,a3がbacklogへ");
        let second = svc.request_all(&b, SizeClass::S256);
        assert_eq!(second.deferred, 2, "プールはa0,a1で満杯のためbも繰延");
        // backlogは最後のrequest (b) の資産のみ
        assert_eq!(svc.backlog_len(), 2);
        let ids: Vec<u64> = lock_backlog(&svc.backlog)
            .items
            .iter()
            .map(|item| item.asset.id.value())
            .collect();
        assert!(ids.contains(&100) && ids.contains(&101), "{ids:?}");
        assert!(
            !ids.contains(&3) && !ids.contains(&4),
            "旧世代a2,a3は不在: {ids:?}"
        );

        // 排液: a0,a1 (プール投入済み旧世代 — 完了許容) + b0,b1のみ
        svc.set_paused(false);
        let ready = events.wait_ready_count(4);
        wait_backlog_drained(&svc);
        let ready_ids: std::collections::HashSet<u64> =
            ready.iter().map(|r| r.asset_id.value()).collect();
        for expected in [1u64, 2, 100, 101] {
            assert!(
                ready_ids.contains(&expected),
                "生成済み: {expected} in {ready_ids:?}"
            );
        }
        assert!(
            !ready_ids.contains(&3) && !ready_ids.contains(&4),
            "置換された旧世代のbacklog資産は生成されない: {ready_ids:?}"
        );
        assert_eq!(svc.stats_dto().generated, 4);
        svc.shutdown();
    }

    /// FG-WP16-3 B3: 並行request_allでもbacklogは常に単一世代 (混在しない)。
    /// 旧実装の「clear後に旧世代itemを逐次append」は競合で混在を生む —
    /// 世代検査付き一括installで構造的に排除する。
    #[test]
    fn concurrent_request_all_never_mixes_generations_in_backlog() {
        let dir = TestDir::new("backlog-concurrent");
        let events = SharedEvents::default();
        let svc = service_with_queue_capacity(&dir, events.clone(), 2);
        svc.set_paused(true); // 全件をbacklog経路に流す
        let mk = |prefix: &str, base_id: u64| -> Vec<AssetSnapshot> {
            (0..48)
                .map(|i| {
                    asset(
                        base_id + u64::try_from(i).unwrap(),
                        &dir.write_png(&format!("{prefix}{i:02}.png"), 64, 64),
                        true,
                    )
                })
                .collect()
        };
        let assets_a = Arc::new(mk("x", 1000));
        let assets_b = Arc::new(mk("y", 2000));
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for assets in [Arc::clone(&assets_a), Arc::clone(&assets_b)] {
            let svc = Arc::clone(&svc);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                svc.request_all(&assets, SizeClass::S256);
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        let ids: Vec<u64> = lock_backlog(&svc.backlog)
            .items
            .iter()
            .map(|item| item.asset.id.value())
            .collect();
        let all_a = ids.iter().all(|id| (1000..1048).contains(id));
        let all_b = ids.iter().all(|id| (2000..2048).contains(id));
        assert!(
            all_a || all_b,
            "backlogは単一世代でなければならない (混在): {ids:?}"
        );
        svc.shutdown();
    }

    // ===== FG-WP16-4 R1/R2: pop→submit窓の決定論的インターリーブ =====

    /// FG-WP16-4 R1: cancelはfeederがpop直後・まだプールへ投入していない
    /// item (inflight) も破棄する。ゲートでfeederをpop直後に停止し、
    /// pop→submit窓でcancelが走っても「not_found報告 → 事後投入」が
    /// 起きないことを決定論的に実証する。
    #[cfg(any(debug_assertions, feature = "devtools"))]
    #[test]
    fn cancel_discards_feeder_inflight_item_in_pop_to_submit_window() {
        let dir = TestDir::new("backlog-inflight-cancel");
        let events = SharedEvents::default();
        let svc = service_with_queue_capacity(&dir, events.clone(), 3);
        svc.set_paused(true); // 決定的にキューを埋める
        let assets: Vec<AssetSnapshot> = (0..4)
            .map(|i| {
                asset(
                    u64::try_from(i).unwrap() + 1,
                    &dir.write_png(&format!("w{i}.png"), 64, 64),
                    true,
                )
            })
            .collect();
        let accepted = svc.request_all(&assets, SizeClass::S256);
        assert_eq!(accepted.queued, 3);
        assert_eq!(accepted.deferred, 1, "w3 (id=4) がbacklogへ");

        // ゲートを先に装填してから投入済み1件をcancel → feederが起床し
        // w3をpopした直後で確定的に停止する
        let gate = svc.debug_arm_feeder_gate();
        let cancel_first = svc.cancel(&assets[0..1], SizeClass::S256);
        assert_eq!(cancel_first.cancelled_queued, 1);
        assert!(gate.wait_reached(), "feederがbacklog先頭 (w3) をpopした");

        // pop→submit窓でcancel(w3): w3はまだプールへ無い
        let cancel_w3 = svc.cancel(&assets[3..4], SizeClass::S256);
        assert_eq!(
            cancel_w3.cancelled_backlog, 1,
            "inflight分を破棄報告: {cancel_w3:?}"
        );
        assert_eq!(cancel_w3.not_found, 0, "{cancel_w3:?}");
        assert_eq!(cancel_w3.cancelled_queued, 0, "{cancel_w3:?}");

        gate.release();
        // w3がプールへ投入されない (キューはw1,w2の2件のまま)
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            svc.stats_dto().queue_depth,
            2,
            "cancel済みw3は事後投入されない"
        );
        assert_eq!(svc.backlog_len(), 0);

        svc.set_paused(false);
        let ready = events.wait_ready_count(2);
        let ready_ids: std::collections::HashSet<u64> =
            ready.iter().map(|r| r.asset_id.value()).collect();
        assert!(
            ready_ids.contains(&2) && ready_ids.contains(&3),
            "{ready_ids:?}"
        );
        assert!(
            !ready_ids.contains(&4),
            "cancel済みw3 (id=4) は生成されない: {ready_ids:?}"
        );
        assert_eq!(svc.stats_dto().generated, 2);
        svc.shutdown();
    }

    /// FG-WP16-4 R2: feederがpopした旧世代itemは、より新しいrequest_allの
    /// 開始後はプールへ投入されない (generation検査は投入**前**)。
    /// ゲート停止中に新世代request_allを実行→解放、で決定論的に実証。
    #[cfg(any(debug_assertions, feature = "devtools"))]
    #[test]
    fn feeder_never_submits_stale_generation_item_after_new_request_all() {
        let dir = TestDir::new("backlog-stale-gen");
        let events = SharedEvents::default();
        let svc = service_with_queue_capacity(&dir, events.clone(), 3);
        svc.set_paused(true);
        let first: Vec<AssetSnapshot> = (0..4)
            .map(|i| {
                asset(
                    u64::try_from(i).unwrap() + 1,
                    &dir.write_png(&format!("g1-{i}.png"), 64, 64),
                    true,
                )
            })
            .collect();
        let accepted = svc.request_all(&first, SizeClass::S256);
        assert_eq!(accepted.queued, 3);
        assert_eq!(accepted.deferred, 1, "旧世代item (g1-3, id=4) がbacklogへ");

        // ゲート装填後に投入済み2件をcancel → feederが旧世代itemをpopした
        // 直後に確定的に停止
        let gate = svc.debug_arm_feeder_gate();
        let cancel_two = svc.cancel(&first[0..2], SizeClass::S256);
        assert_eq!(cancel_two.cancelled_queued, 2);
        assert!(gate.wait_reached(), "feederが旧世代itemをpopした");

        // 停止中に新世代request_all (g2-0): プールに空き1 → 直接投入
        let second = vec![asset(100, &dir.write_png("g2-0.png", 64, 64), true)];
        let accepted2 = svc.request_all(&second, SizeClass::S256);
        assert_eq!(accepted2.queued, 1);
        assert_eq!(accepted2.deferred, 0);

        gate.release();
        std::thread::sleep(Duration::from_millis(300));
        // 旧世代itemは投入されない: キューは g1-2 + g2-0 の2件
        assert_eq!(
            svc.stats_dto().queue_depth,
            2,
            "旧世代itemは新世代開始後に投入されない"
        );

        svc.set_paused(false);
        let ready = events.wait_ready_count(2);
        let ready_ids: std::collections::HashSet<u64> =
            ready.iter().map(|r| r.asset_id.value()).collect();
        assert!(ready_ids.contains(&3), "g1-2 (id=3): {ready_ids:?}");
        assert!(ready_ids.contains(&100), "g2-0 (id=100): {ready_ids:?}");
        assert!(
            !ready_ids.contains(&4),
            "旧世代g1-3 (id=4) は生成されない: {ready_ids:?}"
        );
        assert_eq!(svc.stats_dto().generated, 2);
        svc.shutdown();
    }

    #[test]
    fn request_generates_and_emits_ready_then_second_hits_cache() {
        let dir = TestDir::new("e2e");
        let events = SharedEvents::default();
        let svc = service(&dir, events.clone());
        let a = asset(1, &dir.write_png("a.png", 600, 400), true);
        let b = asset(2, &dir.write_png("b.png", 300, 200), true);

        // 1回目: ミス → 生成 → readyイベント
        let first = svc.request(
            &[a.clone(), b.clone()],
            SizeClass::S256,
            Priority::Background,
        );
        assert_eq!(first.queued, 2);
        assert_eq!(first.cache_hits, 0);
        let ready = events.wait_ready_count(2);
        assert_eq!(ready.len(), 2);
        assert!(ready.iter().all(|r| r.size_class == "s256"));
        assert!(ready.iter().all(|r| r.cache_generation == 1));
        let ids: Vec<u64> = ready.iter().map(|r| r.asset_id.value()).collect();
        assert!(ids.contains(&1) && ids.contains(&2));

        let dto = svc.stats_dto();
        assert_eq!(dto.generated, 2);
        assert_eq!(dto.misses, 2);
        assert_eq!(dto.hits, 0);
        assert_eq!(dto.hit_rate, Some(0.0));
        assert_eq!(dto.queue_depth, 0);

        // 2回目: 全キャッシュヒット (acceptance #2)
        let second = svc.request(&[a, b], SizeClass::S256, Priority::Background);
        assert_eq!(second.cache_hits, 2);
        assert_eq!(second.queued, 0);
        assert_eq!(second.rejected_dup, 0);
        let ready2 = events.wait_ready_count(4);
        assert_eq!(ready2.len(), 4);

        let dto = svc.stats_dto();
        assert_eq!(dto.hits, 2);
        assert_eq!(dto.generated, 2, "2回目で再生成しない");
        assert_eq!(dto.hit_rate, Some(0.5));
        assert!(dto.p50_ms.is_some());
        svc.shutdown();
    }

    #[test]
    fn unreadable_and_unknown_assets_are_skipped() {
        let dir = TestDir::new("skip");
        let events = SharedEvents::default();
        let svc = service(&dir, events.clone());
        let good = asset(1, &dir.write_png("g.png", 64, 64), true);
        let unreadable = AssetSnapshot {
            readable: false,
            ..asset(2, &good.source_path, true)
        };
        let counters = svc.request(&[good, unreadable], SizeClass::S256, Priority::Background);
        assert_eq!(counters.queued, 1);
        assert_eq!(counters.skipped_unreadable, 1);
        events.wait_ready_count(1);
        svc.shutdown();
    }

    #[test]
    fn failure_emits_failed_event() {
        let dir = TestDir::new("fail");
        let events = SharedEvents::default();
        let svc = service(&dir, events.clone());
        let garbage = dir.path().join("garbage.png");
        fs::write(&garbage, b"not a png at all").unwrap();
        let bad = asset(9, &garbage, true);

        let counters = svc.request(&[bad], SizeClass::S256, Priority::Background);
        assert_eq!(counters.queued, 1);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let failed = events.failed_items();
            if !failed.is_empty() {
                assert_eq!(failed[0].asset_id, AssetId::new(9));
                assert_eq!(failed[0].size_class, "s256");
                assert!(
                    failed[0].reason.contains("decode failed"),
                    "{}",
                    failed[0].reason
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for failed event"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(svc.stats_dto().failures, 1);
        svc.shutdown();
    }

    #[test]
    fn paused_pool_and_cancel_emits_nothing() {
        let dir = TestDir::new("cancel");
        let events = SharedEvents::default();
        let svc = service(&dir, events.clone());
        svc.set_paused(true);
        let a = asset(1, &dir.write_png("a.png", 64, 64), true);
        let b = asset(2, &dir.write_png("b.png", 64, 64), true);
        let counters = svc.request(
            &[a.clone(), b.clone()],
            SizeClass::S256,
            Priority::Background,
        );
        assert_eq!(counters.queued, 2);
        assert!(svc.stats_dto().paused);
        std::thread::sleep(Duration::from_millis(200));
        assert!(events.ready_items().is_empty(), "paused pool must not emit");

        let cancel = svc.cancel(&[a.clone(), b.clone()], SizeClass::S256);
        assert_eq!(cancel.cancelled_queued, 2);
        svc.set_paused(false);
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            events.ready_items().is_empty(),
            "cancelled jobs must not emit"
        );
        assert_eq!(svc.stats_dto().failures, 0);
        assert_eq!(svc.stats_dto().generated, 0);

        // 同じ要求をやり直すと正常に生成される (acceptance #5の一部)
        let retry = svc.request(&[a, b], SizeClass::S256, Priority::Background);
        assert_eq!(retry.queued, 2);
        events.wait_ready_count(2);
        svc.shutdown();
    }

    #[test]
    fn reprioritize_counts_for_queued_jobs() {
        let dir = TestDir::new("reprio");
        let events = SharedEvents::default();
        let svc = service(&dir, events.clone());
        svc.set_paused(true);
        let a = asset(1, &dir.write_png("a.png", 64, 64), true);
        svc.request(
            std::slice::from_ref(&a),
            SizeClass::S256,
            Priority::Background,
        );
        let result = svc.reprioritize(
            std::slice::from_ref(&a),
            SizeClass::S256,
            Priority::Interactive,
        );
        assert_eq!(result.updated, 1);
        // 未要求のキー (別ソースパスでキャッシュキーが異なる)
        let other = asset(999, &dir.write_png("other.png", 64, 64), true);
        let result = svc.reprioritize(&[other], SizeClass::S256, Priority::Interactive);
        assert_eq!(result.not_found, 1);
        svc.set_paused(false);
        events.wait_ready_count(1);
        svc.shutdown();
    }

    #[test]
    fn clear_cache_empties_and_bumps_generation() {
        let dir = TestDir::new("clear");
        let events = SharedEvents::default();
        let svc = service(&dir, events.clone());
        let a = asset(1, &dir.write_png("a.png", 64, 64), true);
        svc.request(
            std::slice::from_ref(&a),
            SizeClass::S256,
            Priority::Background,
        );
        events.wait_ready_count(1);
        // readyイベントのgenerationは要求時=1
        assert_eq!(events.ready_items()[0].cache_generation, 1);

        let cleared = svc.clear_cache().unwrap();
        assert_eq!(cleared.removed_files, 1);
        assert!(cleared.removed_bytes > 0);
        assert_eq!(cleared.cache_generation, 2);
        assert_eq!(svc.cache_generation(), 2);

        // 再要求はミス → 再生成 (acceptance #5)
        let after = svc.request(&[a], SizeClass::S256, Priority::Background);
        assert_eq!(after.queued, 1);
        assert_eq!(after.cache_hits, 0);
        events.wait_ready_count(2);
        assert_eq!(events.ready_items()[1].cache_generation, 2);
        svc.shutdown();
    }

    #[test]
    fn many_hits_are_batched_with_cap() {
        let dir = TestDir::new("batch");
        let events = SharedEvents::default();
        let svc = service(&dir, events.clone());
        let assets: Vec<AssetSnapshot> = (0..25)
            .map(|i| {
                asset(
                    u64::try_from(i).unwrap() + 1,
                    &dir.write_png(&format!("p{i:02}.png"), 64, 64),
                    true,
                )
            })
            .collect();
        // 初回生成
        svc.request(&assets, SizeClass::S256, Priority::Background);
        events.wait_ready_count(25);
        events.clear();
        // 25件同時ヒット → ImmediateReady 25件 → 20件+5件に分割されるはず
        let counters = svc.request(&assets, SizeClass::S256, Priority::Background);
        assert_eq!(counters.cache_hits, 25);
        events.wait_ready_count(25);
        let batches: Vec<usize> = events
            .batches()
            .into_iter()
            .map(|b| match b {
                RecordedBatch::Ready(items) => items.len(),
                RecordedBatch::Failed(_) => 0,
            })
            .filter(|n| *n > 0)
            .collect();
        assert_eq!(batches.iter().sum::<usize>(), 25);
        assert!(batches.iter().all(|n| *n <= EVENT_BATCH_MAX), "{batches:?}");
        assert!(
            batches.len() >= 2,
            "25 items must split into >=2 batches: {batches:?}"
        );
        svc.shutdown();
    }

    // ===== 開発用エラー注入 (devtools機能。debug/devtoolsビルドのみ) =====

    #[cfg(any(debug_assertions, feature = "devtools"))]
    #[test]
    fn failure_injection_fails_individually_and_recovers_at_zero_rate() {
        let dir = TestDir::new("inject");
        let events = SharedEvents::default();
        let svc = service(&dir, events.clone());
        let a = asset(1, &dir.write_png("a.png", 64, 64), true);
        let b = asset(2, &dir.write_png("b.png", 64, 64), true);

        // decode全件注入: 個別failedイベントのみ。キューは停滞しない
        svc.set_injection(gento_image::thumbnail::InjectionMode::Decode, 1.0);
        let counters = svc.request(
            &[a.clone(), b.clone()],
            SizeClass::S256,
            Priority::Background,
        );
        assert_eq!(counters.queued, 2);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if events.failed_items().len() >= 2 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for injected failures"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let failed = events.failed_items();
        assert!(failed.iter().all(|f| f.reason.contains("injected")));
        assert_eq!(events.ready_items().len(), 0);
        assert_eq!(svc.stats_dto().failures, 2);
        assert_eq!(
            svc.stats_dto().queue_depth,
            0,
            "queue must drain, not stall"
        );
        assert_eq!(svc.stats_dto().injection_mode, "decode");
        assert_eq!(svc.stats_dto().injection_rate, 1.0);

        // rate=0で復帰: 同じ要求が成功する
        svc.set_injection(gento_image::thumbnail::InjectionMode::None, 0.0);
        let retry = svc.request(
            &[a.clone(), b.clone()],
            SizeClass::S256,
            Priority::Background,
        );
        assert_eq!(retry.queued, 2);
        events.wait_ready_count(2);
        assert_eq!(svc.stats_dto().generated, 2);

        // cache_write全件注入: failed + 一時ファイル残存なし
        svc.set_injection(gento_image::thumbnail::InjectionMode::CacheWrite, 1.0);
        let c = asset(3, &dir.write_png("c.png", 300, 200), true);
        let counters = svc.request(&[c], SizeClass::S256, Priority::Background);
        assert_eq!(counters.queued, 1);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if events
                .failed_items()
                .iter()
                .any(|f| f.asset_id == AssetId::new(3))
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for cache_write failure"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut tmp_files = 0;
        for shard in fs::read_dir(dir.path().join("cache")).unwrap() {
            let shard = shard.unwrap();
            if shard.file_type().unwrap().is_dir() {
                for file in fs::read_dir(shard.path()).unwrap() {
                    let name = file.unwrap().file_name().to_string_lossy().into_owned();
                    if name.starts_with(".tmp-") {
                        tmp_files += 1;
                    }
                }
            }
        }
        assert_eq!(
            tmp_files, 0,
            "injected cache_write failure must not leave temps"
        );
        svc.shutdown();
    }

    #[test]
    fn stats_dto_reports_queue_depths_per_priority() {
        let dir = TestDir::new("depths");
        let events = SharedEvents::default();
        let svc = service(&dir, events.clone());
        svc.set_paused(true);
        let assets: Vec<AssetSnapshot> = (0..4)
            .map(|i| asset(i + 1, &dir.write_png(&format!("p{i}.png"), 64, 64), true))
            .collect();
        svc.request(&[assets[0].clone()], SizeClass::S256, Priority::Interactive);
        svc.request(&[assets[1].clone()], SizeClass::S256, Priority::Viewport);
        svc.request(
            &[assets[2].clone(), assets[3].clone()],
            SizeClass::S256,
            Priority::Background,
        );
        let dto = svc.stats_dto();
        assert_eq!(dto.queue_depth, 4);
        assert_eq!(dto.queue_depths.interactive, 1);
        assert_eq!(dto.queue_depths.viewport, 1);
        assert_eq!(dto.queue_depths.background, 2);
        assert_eq!(dto.queue_depths.prefetch + dto.queue_depths.overscan, 0);
        svc.set_paused(false);
        events.wait_ready_count(4);
        let dto = svc.stats_dto();
        assert_eq!(dto.queue_depth, 0);
        assert_eq!(
            dto.queue_depths.interactive + dto.queue_depths.background,
            0
        );
        svc.shutdown();
    }

    // ===== 内容感応バージョン (FG-1 / F3) =====

    #[test]
    fn ready_event_version_changes_when_source_file_changes() {
        // 元ファイル変更 (内容違い→size/mtime変化) → 再要求 (rescan相当) で
        // 同一assetIdのままreadyイベントのversionが変わること。
        // URLが内容感応になることのRust側証明 (§12-5)。
        let dir = TestDir::new("version");
        let events = SharedEvents::default();
        let svc = service(&dir, events.clone());
        let png = dir.write_png("a.png", 64, 64);
        let a = asset(1, &png, true);
        svc.request(
            std::slice::from_ref(&a),
            SizeClass::S256,
            Priority::Background,
        );
        let ready1 = events.wait_ready_count(1);
        let v1 = ready1[0].version;
        assert!(v1.to_string().len() <= 18, "v={v1}");

        // 元ファイルを別内容へ書き換える (size/mtimeが変わる)
        let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(96, 48, |x, y| {
            image::Rgba([(x % 251) as u8, (y % 241) as u8, 64, 255])
        }));
        img.save(&png).unwrap();
        let changed = asset(1, &png, true); // 同一ID、新しいsize/mtime
        let counters = svc.request(
            std::slice::from_ref(&changed),
            SizeClass::S256,
            Priority::Background,
        );
        assert_eq!(counters.queued, 1, "新しいキャッシュキーでミスになる");
        let ready2 = events.wait_ready_count(2);
        let item = ready2
            .iter()
            .rev()
            .find(|r| r.asset_id == AssetId::new(1))
            .expect("second ready for asset 1");
        assert_ne!(
            item.version, v1,
            "元ファイル変更でversion (=URL ?v=) が変わること"
        );
        // cache_generationはクリアしていないため不変 (versionとは独立)
        assert_eq!(item.cache_generation, ready1[0].cache_generation);
        svc.shutdown();
    }

    #[test]
    fn cache_hit_ready_carries_version_too() {
        // 即時ready (キャッシュヒット) にもversionが乗ること
        let dir = TestDir::new("version-hit");
        let events = SharedEvents::default();
        let svc = service(&dir, events.clone());
        let a = asset(1, &dir.write_png("a.png", 64, 64), true);
        svc.request(
            std::slice::from_ref(&a),
            SizeClass::S256,
            Priority::Background,
        );
        let first = events.wait_ready_count(1);
        events.clear();
        let second = svc.request(&[a], SizeClass::S256, Priority::Background);
        assert_eq!(second.cache_hits, 1);
        let hits = events.wait_ready_count(1);
        assert_eq!(hits[0].version, first[0].version);
        svc.shutdown();
    }
}
