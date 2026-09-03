//! サムネイル生成サービスとcommand群 (Phase 4 / WP-3)。
//!
//! gento-image (pool/cache/stats) を組み合わせたTauri thin adapter。
//! - 要求ごとにキャッシュlookup → ヒットなら即時 `thumb:ready`
//! - ミスならプールへ投入 (dedup・優先度・上限付き)
//! - 完了通知はバッチング (100ms or 20件) して `thumb:ready` / `thumb:failed`
//! - 統計・一時停止・キャッシュ全削除 (cache_generation更新)
//!
//! 契約: docs/dev/api-contract.md §7以降。

use std::collections::{HashMap, VecDeque};
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
#[derive(Debug, Clone)]
struct BacklogItem {
    asset: AssetSnapshot,
    size_class: SizeClass,
    priority: Priority,
}

/// backlogの状態 (drain報告用カウンタ込み)。
#[derive(Debug, Default)]
struct BacklogState {
    items: VecDeque<BacklogItem>,
    /// 現在の要求で繰延された総数 (報告用。置換時にリセット)。
    deferred_total: u64,
    /// 現在の要求でbacklogから処理 (投入・統合・フィード時ヒット) した数。
    processed: u64,
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
    pub fn request_all(&self, assets: &[AssetSnapshot], size_class: SizeClass) -> RequestAccepted {
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
            *backlog = BacklogState::default();
        }
        let mut counters = RequestAccepted::default();
        let mut immediate = Vec::new();
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
                    let mut backlog = lock_backlog(&self.backlog);
                    backlog.items.push_back(BacklogItem {
                        asset: asset.clone(),
                        size_class,
                        priority: Priority::Background,
                    });
                    backlog.deferred_total += 1;
                    counters.deferred += 1;
                }
                EnqueueOutcome::Shutdown => counters.rejected_full += 1,
            }
        }
        send_immediate(&self.core.emitter_tx, immediate);
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
    pub fn cancel(&self, assets: &[AssetSnapshot], size_class: SizeClass) -> CancelResult {
        let mut result = CancelResult::default();
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
                CancelOutcome::NotFound => result.not_found += 1,
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
            let item = lock_backlog(backlog).items.pop_front();
            let Some(item) = item else {
                break;
            };
            taken += 1;
            match core.enqueue_asset(&item.asset, item.size_class, item.priority) {
                EnqueueOutcome::Queued | EnqueueOutcome::Merged => {
                    lock_backlog(backlog).processed += 1;
                }
                EnqueueOutcome::CacheHit(hit) => {
                    lock_backlog(backlog).processed += 1;
                    immediate.push(hit);
                }
                EnqueueOutcome::Full => {
                    // 他の投入と競合して満杯: 先頭へ戻して次の完了で再試行
                    lock_backlog(backlog).items.push_front(item);
                    hit_full = true;
                    break;
                }
                EnqueueOutcome::Shutdown => {
                    // プール停止: 以降の再投入は無意味 (残りは停止時報告)
                    lock_backlog(backlog).items.push_front(item);
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
