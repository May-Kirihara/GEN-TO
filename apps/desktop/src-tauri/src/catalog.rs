//! カタログcommand群とバックグラウンド走査 (Phase 2 / WP-2)。
//!
//! 本モジュールはthin adapter: ビジネスロジックはgento-core / gento-imageにある。
//! UIスレッドでファイルI/Oを行わない (走査・プローブは専用スレッド)。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
#[cfg(any(debug_assertions, feature = "devtools"))]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

#[cfg(any(debug_assertions, feature = "devtools"))]
use gento_core::catalog::{build_virtual, VIRTUAL_CATALOG_COUNT};
use gento_core::catalog::{
    sort_records_by, CatalogMode, CatalogSort, CatalogState, ImageRecord, ImageState, PageError,
    PageResult, ThumbnailState, Triage,
};
use gento_core::library::decode_library_asset_id;
use gento_core::AssetId;
use gento_image::cache::asset_version;
use gento_image::format::{probe_image, ImageProbe};
use gento_image::metrics::{log_scan_metrics, read_self_rss_kb, ScanMetrics};
use gento_image::scan::{scan_images_batch, ScanBatch, ScanEntryError, SCAN_MAX_FILES};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

/// 進捗イベントに載せる直近エラーサンプルの上限 (イベント嵐対策)。
const ERROR_SAMPLE_LIMIT: usize = 5;

/// Tauri command間で共有する実行時状態。
pub struct AppState {
    /// 現在閲覧中のフォルダ (一時走査の対象。`open_folder` で変更)。
    pub dataset_path: Mutex<PathBuf>,
    pub catalog: Mutex<CatalogInner>,
    /// サムネイル生成サービス (WP-3)。setupで生成、Exitでshutdown。
    pub thumbnails: Option<Arc<crate::thumbnails::ThumbnailService>>,
    /// ローカル画像プロトコルの統計 (WP-4)。
    pub proto_stats: Arc<crate::protocol::ProtoStats>,
    /// SQLite DB状態 (WP-1)。setupで生成。開けなくてもアプリは続行
    /// (通常ファイラー機能はDB非依存のため)。
    pub db: Option<crate::db::DbState>,
    /// 登録ルート制御 (WP-2)。DBが開けている場合のみ生成。
    pub library: Option<Arc<crate::library::LibraryController>>,
    /// ファイル操作サービス (WP-7)。DBとlibraryが開けている場合のみ生成。
    pub file_ops: Option<Arc<crate::fileops::FileOpsService>>,
    /// 計測ブリッジ (devtest) の有効フラグ (FG-1)。devtoolsビルドのみ存在
    /// (WP-0)。起動時に環境変数 `GENTO_DEV_BRIDGE` (値は厳密に "1") から
    /// 確定し以後不変。危険コマンド (set_failure_injection / devtest_report /
    /// dev_bridge_enabled) のゲートとして使う。
    #[cfg(any(debug_assertions, feature = "devtools"))]
    pub dev_bridge: AtomicBool,
}

/// ロック内で保持するカタログ実状態。
pub struct CatalogInner {
    /// 稼働モードに応じた「表示中」レコード列。
    state: CatalogState,
    /// 最後に完了した一時走査の実レコード (Realモード復帰用。
    /// WP-2でset_catalog_sourceがproduction経路でも使うため無条件保持)。
    real: Vec<ImageRecord>,
    /// 登録ルート由来のスナップショット (WP-2。DBから再生成可能)。
    library: Vec<ImageRecord>,
    /// コレクション表示中の対象 (WP-6)。Someの間は `state.records` が
    /// コレクションビュー (manual=position順 / smart=評価結果) であり、
    /// `library` スナップショット更新の自動反映を**行わない**
    /// (asset:updatedのin-placeパッチのみ適用。再評価は明示
    /// `smart_collection_refresh` / 再選択 — トリガー連鎖回避の設計)。
    collection: Option<CollectionView>,
    /// 表示ソート (WP-4。走査バッチ・ソース切替・スナップショット反映に適用)。
    sort: CatalogSort,
    /// 現在の一時走査トークン (世代。QA-WP4)。`begin_scan` で進み、
    /// 旧トークンを持つ走査ワーカーの結果は破棄される。
    scan_token: u64,
    progress: ProgressCounters,
    scan_started_at: Option<Instant>,
    last_scan_elapsed_ms: Option<u64>,
}

/// コレクション表示の対象識別 (WP-6)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollectionView {
    pub collection_id: i64,
}

#[derive(Default)]
struct ProgressCounters {
    scanned: u64,
    found: u64,
    errors: u64,
    /// 直近 `ERROR_SAMPLE_LIMIT` 件のみ保持。
    error_samples: Vec<String>,
}

impl ProgressCounters {
    fn push_error(&mut self, message: String) {
        self.errors += 1;
        self.error_samples.push(message);
        if self.error_samples.len() > ERROR_SAMPLE_LIMIT {
            let excess = self.error_samples.len() - ERROR_SAMPLE_LIMIT;
            self.error_samples.drain(0..excess);
        }
    }
}

impl CatalogInner {
    fn new() -> Self {
        Self {
            state: CatalogState::new(),
            real: Vec::new(),
            library: Vec::new(),
            collection: None,
            sort: CatalogSort::default(),
            scan_token: 0,
            progress: ProgressCounters::default(),
            scan_started_at: None,
            last_scan_elapsed_ms: None,
        }
    }

    /// 現在表示中のコレクション (WP-6)。非表示中はNone。
    pub fn collection_view(&self) -> Option<CollectionView> {
        self.collection
    }

    /// 現在のソートモードで表示レコードを並べ替える。
    fn apply_sort(&mut self) {
        sort_records_by(&mut self.state.records, self.sort);
    }
}

impl AppState {
    pub fn new(dataset_path: PathBuf) -> Self {
        Self {
            dataset_path: Mutex::new(dataset_path),
            catalog: Mutex::new(CatalogInner::new()),
            thumbnails: None,
            proto_stats: Arc::new(crate::protocol::ProtoStats::new()),
            db: None,
            library: None,
            file_ops: None,
            #[cfg(any(debug_assertions, feature = "devtools"))]
            dev_bridge: AtomicBool::new(crate::devtools::dev_bridge_enabled_strict()),
        }
    }

    /// 現在閲覧中フォルダのコピー (poisoning復旧付き)。
    pub fn current_folder(&self) -> PathBuf {
        self.dataset_path
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// カタログロックの取得 (poisoning復旧付き)。
    pub fn lock_catalog(&self) -> MutexGuard<'_, CatalogInner> {
        lock(&self.catalog)
    }

    /// 計測ブリッジが有効か (起動時に確定したフラグ)。devtoolsビルドのみ。
    #[cfg(any(debug_assertions, feature = "devtools"))]
    pub fn dev_bridge_enabled(&self) -> bool {
        self.dev_bridge.load(Ordering::SeqCst)
    }

    /// サムネイルサービスを取り出す。setup以降は必ず存在する。
    pub fn thumbnails(&self) -> Arc<crate::thumbnails::ThumbnailService> {
        self.thumbnails
            .as_ref()
            .expect("ThumbnailService is created in setup")
            .clone()
    }
}

impl CatalogInner {
    /// 現在の稼働モード (WP-5: metadataコマンドのモードガード用)。
    pub fn mode(&self) -> CatalogMode {
        self.state.mode
    }

    /// IDからレコードを引く (線形探索。コマンド単位の規模想定)。
    pub fn record(&self, id: AssetId) -> Option<&ImageRecord> {
        self.state.records.iter().find(|r| r.id == id)
    }

    /// 表示中カタログのレコード列 (スナップショット抽出用)。
    pub fn records(&self) -> &[ImageRecord] {
        &self.state.records
    }

    /// ライブラリAssetのメタデータ編集を表示済みレコードへ反映する (WP-5)。
    ///
    /// - `inner.library` (DB由来スナップショット) は常に更新
    /// - `inner.state.records` はLibrary表示中のみ更新 (Real/Virtualの
    ///   レコードはadhoc採番IDで数値が偶発一致する可能性があるため触らない)
    /// - `inner.real` (一時走査) は更新しない (同上)
    ///
    /// generationを進めない: ページ再取得は不要で、フロントは
    /// `asset:updated` イベントでローカルコピーを更新する。既存generationの
    /// ページ取得は更新済みレコードから値を返す。
    /// テスト用: カタログモードを強制設定する (metadataコマンドの
    /// Library/Real/Virtualガード検証。productionはset_catalog_source経由)。
    #[cfg(test)]
    pub(crate) fn force_mode_for_test(&mut self, mode: CatalogMode) {
        self.state.mode = mode;
    }

    /// テスト用: Libraryモード表示レコードとlibraryスナップショットへ
    /// レコードを追加する (メタデータ編集のin-place反映検証用)。
    #[cfg(test)]
    pub(crate) fn push_library_record_for_test(&mut self, record: ImageRecord) {
        if self.state.mode != CatalogMode::Library {
            self.state.mode = CatalogMode::Library;
        }
        self.state.records.push(record);
        self.library = self.state.records.clone();
    }

    pub fn patch_library_assets(&mut self, asset_ids: &[i64], f: impl Fn(&mut ImageRecord)) {
        use gento_core::library::{decode_library_asset_id, LibraryAssetRef};
        let wanted: std::collections::HashSet<i64> = asset_ids.iter().copied().collect();
        let matches = |record: &ImageRecord| match decode_library_asset_id(record.id) {
            LibraryAssetRef::Asset(row_id) => wanted.contains(&row_id),
            LibraryAssetRef::ProvisionalFile(_) => false,
        };
        for record in self.library.iter_mut().filter(|r| matches(r)) {
            f(record);
        }
        // Library系表示 (通常ライブラリ + コレクションビュー) のレコードへ
        // in-place適用。コレクションビューのレコードもstable AssetIdを持つ
        // ため同じパッチ経路で追従する (WP-6)。
        if self.state.mode == CatalogMode::Library && !self.state.scanning {
            for record in self.state.records.iter_mut().filter(|r| matches(r)) {
                f(record);
            }
        }
    }

    /// コレクションビューへ切替える (WP-6)。`records` は呼び出し側が構築した
    /// 表示順 (manual=position順 / smart=評価順)。**ソートは適用しない**
    /// (表示順の正本はコレクション)。generationを進めて新しいgenerationを返す。
    pub fn set_collection_display(
        &mut self,
        view: CollectionView,
        records: Vec<ImageRecord>,
    ) -> u64 {
        self.collection = Some(view);
        self.state.mode = CatalogMode::Library;
        self.state.scanning = false;
        self.state.records = records;
        self.state.generation += 1;
        self.state.generation
    }

    /// コレクション表示を終了し、通常ライブラリ表示へ戻す (WP-6)。
    /// generationを進めて新しいgenerationを返す (表示が存在しない場合は
    /// 何もせず0)。
    pub fn clear_collection_display(&mut self) -> u64 {
        if self.collection.take().is_none() {
            return 0;
        }
        self.state.mode = CatalogMode::Library;
        self.state.records = self.library.clone();
        self.apply_sort();
        self.state.generation += 1;
        self.state.generation
    }
}

/// `get_catalog_summary` の戻り値 (docs/dev/api-contract.md 契約)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CatalogSummary {
    pub generation: u64,
    pub mode: CatalogMode,
    pub total: u32,
    pub scanning: bool,
    pub scanned_count: u64,
    pub error_count: u64,
    pub last_scan_elapsed_ms: Option<u64>,
    /// 現在の表示ソート (WP-4。"name" | "date_desc" | "size_desc")。
    pub sort: CatalogSort,
}

/// `scan:progress` イベントペイロード。
/// WP-2でライブラリスキャン用フィールドを追加 (一時走査では 0/空)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScanProgressPayload {
    pub scanned: u64,
    pub found: u64,
    pub errors: u64,
    pub elapsed_ms: u64,
    pub done: bool,
    /// 直近数件のエラーサンプル (毎件は流さない)。
    pub error_samples: Vec<String>,
    /// 変更検出数 (ライブラリスキャンのみ)。
    pub updated: u64,
    /// 外部移動・改名の再発見数 (同上)。
    pub relocated: u64,
    /// 不在化数 (同上)。
    pub missing: u64,
    /// ハッシュ計算完了数 (同上)。
    pub hashed: u64,
    /// ハッシュ待ち残数 (同上)。
    pub pending_hash: u64,
    /// `"scanning"` | `"hashing"` (一時走査ではNone)。
    pub phase: Option<String>,
}

/// `catalog:changed` イベントの理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogChangeReason {
    ScanBatch,
    ScanDone,
    ModeSwitch,
    /// ソート変更 (WP-4)。表示順のみ変化 (generationは進む)。
    SortChange,
    /// コレクション表示の開始・切替 (WP-6)。
    CollectionSwitch,
    /// 表示中コレクションの再評価・項目変更反映 (WP-6)。
    CollectionRefresh,
}

/// `catalog:changed` イベントペイロード。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CatalogChangedPayload {
    pub generation: u64,
    pub reason: CatalogChangeReason,
}

/// poisoning時も内容を復旧扱いにする (走査スレッドpanicでUIが固まらないよう)。
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn summary_of(inner: &CatalogInner) -> CatalogSummary {
    let last_scan_elapsed_ms = if inner.state.scanning {
        inner
            .scan_started_at
            .map(|t| t.elapsed().as_millis() as u64)
    } else {
        inner.last_scan_elapsed_ms
    };
    CatalogSummary {
        generation: inner.state.generation,
        mode: inner.state.mode,
        total: inner.state.total(),
        scanning: inner.state.scanning,
        scanned_count: inner.progress.scanned,
        error_count: inner.progress.errors,
        last_scan_elapsed_ms,
        sort: inner.sort,
    }
}

/// summary_ofのcrate内公開ラッパ (collections.rsのcollection_show応答用)。
pub(crate) fn summary_of_pub(inner: &CatalogInner) -> CatalogSummary {
    summary_of(inner)
}

/// 走査開始の状態遷移。旧走査トークンを無効化し (走査中でも再投入可能。
/// QA-WP4)、新トークンを返す。コレクション表示も終了する (Realモードへ
/// 戻るため。WP-6)。
fn begin_scan(inner: &mut CatalogInner) -> (u64, u64) {
    inner.state.scanning = true;
    // 再走査はカタログを置き換えるためRealモードへ戻し、内容をクリアする
    inner.state.records.clear();
    inner.state.mode = CatalogMode::Real;
    inner.collection = None;
    inner.state.generation += 1;
    inner.scan_token += 1;
    inner.progress = ProgressCounters::default();
    inner.scan_started_at = Some(Instant::now());
    inner.last_scan_elapsed_ms = None;
    (inner.state.generation, inner.scan_token)
}

/// 走査バッチをカタログへ反映する。トークンが無効化されていたら反映せず
/// falseを返す (呼び出し元の走査ワーカーは走査を中止する)。
fn apply_scan_batch(
    inner: &mut CatalogInner,
    token: u64,
    records: Vec<ImageRecord>,
    entries_walked: u64,
    probe_failures: &[String],
) -> bool {
    if token != inner.scan_token {
        return false;
    }
    inner.progress.scanned = entries_walked;
    inner.progress.found += records.len() as u64;
    for failure in probe_failures {
        inner.progress.push_error(failure.clone());
    }
    inner.state.records.extend(records);
    inner.apply_sort();
    true
}

/// 走査完了の確定 (`scanning=false`・実レコード確定・最終進捗)。
/// トークンが無効化されていたら確定せず None (新走査が状態を支配中)。
fn finish_scan(
    inner: &mut CatalogInner,
    token: u64,
    scan_elapsed_ms: u64,
    entries: u64,
    errors: &[ScanEntryError],
    truncated: bool,
) -> Option<ScanProgressPayload> {
    if token != inner.scan_token {
        return None;
    }
    inner.state.scanning = false;
    inner.progress.scanned = entries;
    for error in errors {
        inner
            .progress
            .push_error(format!("{}: {}", error.path.display(), error.reason));
    }
    if truncated {
        inner.progress.push_error(format!(
            "件数上限 ({SCAN_MAX_FILES}件) に達したため走査を打ち切りました"
        ));
    }
    inner.last_scan_elapsed_ms = Some(scan_elapsed_ms);
    // Realモード復帰用に実レコードを保持する (set_catalog_sourceが
    // production経路で使用するため無条件。仮想カタログのソースでもある)。
    inner.real = inner.state.records.clone();
    Some(ScanProgressPayload {
        scanned: inner.progress.scanned,
        found: inner.progress.found,
        errors: inner.progress.errors,
        elapsed_ms: scan_elapsed_ms,
        done: true,
        error_samples: inner.progress.error_samples.clone(),
        updated: 0,
        relocated: 0,
        missing: 0,
        hashed: 0,
        pending_hash: 0,
        phase: None,
    })
}

/// 一時走査のキャンセル確定 (QA-WP4)。トークンを無効化して走査ワーカーを
/// 停止させ、確定済みの部分結果はそのまま表示に残す。
/// 走査中でなければ None (何もしない)。
fn cancel_scan_transition(inner: &mut CatalogInner) -> Option<ScanProgressPayload> {
    if !inner.state.scanning {
        return None;
    }
    inner.scan_token += 1;
    inner.state.scanning = false;
    if let Some(started) = inner.scan_started_at.take() {
        inner.last_scan_elapsed_ms = Some(started.elapsed().as_millis() as u64);
    }
    inner.real = inner.state.records.clone();
    Some(ScanProgressPayload {
        scanned: inner.progress.scanned,
        found: inner.progress.found,
        errors: inner.progress.errors,
        elapsed_ms: inner.last_scan_elapsed_ms.unwrap_or_default(),
        done: true,
        error_samples: inner.progress.error_samples.clone(),
        updated: 0,
        relocated: 0,
        missing: 0,
        hashed: 0,
        pending_hash: 0,
        phase: None,
    })
}

// ===== カタログモード切替 (devtools機能。仮想5万件カタログは計測用) =====
// 通常releaseビルドでは set_catalog_mode commandごと存在しない (WP-0)。

#[derive(Debug)]
pub(crate) enum ModeSwitchOutcome {
    Changed(CatalogSummary),
    /// 同一モードへの切替は何も変えない (generation不変)。
    Unchanged(CatalogSummary),
}

#[cfg(any(debug_assertions, feature = "devtools"))]
fn switch_mode(inner: &mut CatalogInner, want_virtual: bool) -> Result<ModeSwitchOutcome, String> {
    if inner.state.scanning {
        return Err("走査中はカタログモードを切替できません".to_string());
    }
    if inner.state.mode == CatalogMode::Library {
        // devtoolsのreal↔virtual切替は一時走査カタログ上の操作。
        // Library表示中は set_catalog_source で temp へ戻してから。
        return Err(
            "仮想カタログへ切替する前に一時走査へ戻してください (set_catalog_source)".to_string(),
        );
    }
    let target = if want_virtual {
        CatalogMode::Virtual
    } else {
        CatalogMode::Real
    };
    if inner.state.mode == target {
        return Ok(ModeSwitchOutcome::Unchanged(summary_of(inner)));
    }
    if want_virtual {
        if inner.real.is_empty() {
            return Err(
                "実カタログが空です。仮想カタログへ切替する前にスキャンを実行してください"
                    .to_string(),
            );
        }
        inner.state.records =
            build_virtual(&inner.real, VIRTUAL_CATALOG_COUNT).map_err(|e| e.to_string())?;
    } else {
        inner.state.records = inner.real.clone();
    }
    inner.state.mode = target;
    inner.state.generation += 1;
    Ok(ModeSwitchOutcome::Changed(summary_of(inner)))
}
/// カタログソース切替の状態遷移 (set_catalog_source本体)。
/// コレクション表示中はmodeが既にLibraryでも実質表示が異なるため
/// (state.recordsがコレクションビュー)、「同一モード=変化なし」の
/// 早期リターンから復帰経路へ回す (WP-6)。
pub(crate) fn switch_catalog_source(
    inner: &mut CatalogInner,
    target: CatalogMode,
) -> ModeSwitchOutcome {
    let exited_collection = inner.collection.take().is_some();
    if inner.state.mode == target && !exited_collection {
        return ModeSwitchOutcome::Unchanged(summary_of(inner));
    }
    match target {
        CatalogMode::Real => inner.state.records = inner.real.clone(),
        CatalogMode::Library => inner.state.records = inner.library.clone(),
        CatalogMode::Virtual => unreachable!("target is Real|Library here"),
    }
    inner.state.mode = target;
    // ソース切替後は現在のソートモードで並べ替える (WP-4)
    inner.apply_sort();
    inner.state.generation += 1;
    ModeSwitchOutcome::Changed(summary_of(inner))
}

/// カタログ表示ソースの切替 (WP-2。production command)。
///
/// - `temp`: 一時走査カタログ (Realモード。`--dataset` の走査結果)
/// - `library`: 登録ルートのDB由来スナップショット (Libraryモード)
///
/// 一時走査の実行中は切り替えを拒否する (run_scanがrecordsを直接更新する
/// ため、Library表示と混在しないようにする)。
#[tauri::command]
pub fn set_catalog_source(
    app: AppHandle,
    state: State<'_, AppState>,
    source: String,
) -> Result<CatalogSummary, String> {
    let target = match source.as_str() {
        "temp" => CatalogMode::Real,
        "library" => CatalogMode::Library,
        other => return Err(format!("不明なカタログソースです: {other} (temp|library)")),
    };
    let outcome = {
        let mut inner = lock(&state.catalog);
        if inner.state.scanning {
            return Err("走査中はカタログソースを切替できません".to_string());
        }
        switch_catalog_source(&mut inner, target)
    };
    if let ModeSwitchOutcome::Changed(summary) = outcome {
        let _ = app.emit(
            "catalog:changed",
            CatalogChangedPayload {
                generation: summary.generation,
                reason: CatalogChangeReason::ModeSwitch,
            },
        );
        Ok(summary)
    } else {
        match outcome {
            ModeSwitchOutcome::Unchanged(summary) => Ok(summary),
            ModeSwitchOutcome::Changed(_) => unreachable!("handled above"),
        }
    }
}

/// ライブラリスナップショットをカタログへ反映する (WP-2)。
/// Libraryモード表示中は表示レコードを差し替え、generationを進めて
/// `catalog:changed` をemitする (ページ再取得でAssetIdの確定へ追従)。
/// コレクション表示中 (WP-6) は `library` スナップショットのみ更新する
/// — コレクションビューの内容は明示refresh・再選択まで保持する
/// (asset:updatedのトリガー連鎖を避ける設計。collections.rs参照)。
pub fn apply_library_snapshot(app: &AppHandle, records: Vec<ImageRecord>) {
    let state = app.state::<AppState>();
    let generation = {
        let mut inner = lock(&state.catalog);
        inner.library = records;
        if inner.state.mode == CatalogMode::Library
            && !inner.state.scanning
            && inner.collection.is_none()
        {
            inner.state.records = inner.library.clone();
            inner.apply_sort();
            inner.state.generation += 1;
            inner.state.generation
        } else {
            0
        }
    };
    if generation > 0 {
        let _ = app.emit(
            "catalog:changed",
            CatalogChangedPayload {
                generation,
                reason: CatalogChangeReason::ScanBatch,
            },
        );
    }
}

#[tauri::command]
pub fn get_catalog_summary(state: State<'_, AppState>) -> CatalogSummary {
    summary_of(&lock(&state.catalog))
}

/// `get_catalog_page` の項目DTO。gento-core `ImageListItem` + 内容感応version
/// (FG-1 / F3) + 情報パネル用フィールド (WP-4)。
/// version計算はキャッシュキー入力 (path+size+mtime) を必要と
/// するため、gento-coreではなくthin adapter層で付与する。
/// camelCaseは `ImageListItem` (本体計画 §9) に合わせる。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogItemDto {
    pub id: AssetId,
    pub display_name: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub byte_size: u64,
    pub state: ImageState,
    pub thumbnail_state: ThumbnailState,
    /// アニメーション付き画像か (WP-3。ビューアはoriginalを直接表示)。
    pub is_animated: bool,
    /// 内容感応URLバージョン (api-contract §12)。元ファイルの
    /// size/mtime・実体パス・アルゴリズムバージョンから決まる。
    /// フロントは original 表示やURL組み立てに使う。
    pub version: u64,
    /// 実ファイルの絶対パス (情報パネル・OS連携。WP-4)。
    pub path: String,
    /// 更新日時 (Unixエポックms。情報パネル。WP-4)。
    pub mtime_ms: u64,
    /// プローブ由来MIME (未プローブはnull。WP-4)。
    pub mime_type: Option<String>,
    /// DB上の位置づけ (WP-4)。library表示: Asset確定済み / 仮ID(ハッシュ待ち)。
    /// 一時走査・仮想: 未登録 (DB外)。
    pub db_state: ItemDbState,
    /// 選別状態 (WP-4。セッションの採用/除外カウント用。編集UIはWP-5)。
    pub triage: Triage,
    /// 星評価 0..=5 (WP-5。グリッドセルの星バッジ表示)。
    pub rating: u8,
    /// お気に入り (WP-5。グリッドセルの♥バッジ表示)。
    pub favorite: bool,
}

/// 項目のDB状態 (WP-4。AssetId符号化 gento-core library に基づく)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemDbState {
    /// 登録済みでAsset確定 (タグ・評価等のライブラリ機能が有効)。
    Asset,
    /// 登録済みだがハッシュ未完了 (Asset未確定の仮ID)。
    Provisional,
    /// 未登録 (一時走査・仮想カタログ)。
    Adhoc,
}

impl ItemDbState {
    fn of(mode: CatalogMode, id: AssetId) -> Self {
        match mode {
            CatalogMode::Library => match decode_library_asset_id(id) {
                gento_core::library::LibraryAssetRef::Asset(_) => ItemDbState::Asset,
                gento_core::library::LibraryAssetRef::ProvisionalFile(_) => {
                    ItemDbState::Provisional
                }
            },
            CatalogMode::Real | CatalogMode::Virtual => ItemDbState::Adhoc,
        }
    }
}

/// `get_catalog_page` の戻り値DTO (`PageResult` + 項目のversion)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CatalogPageDto {
    pub generation: u64,
    pub total: u32,
    pub offset: u32,
    pub items: Vec<CatalogItemDto>,
    pub has_more: bool,
}

/// ページ項目へ内容感応versionと情報パネル用フィールドを付与する。
/// レコード列を1回走査してページ内IDの補助表を作る (項目ごとの線形探索を
/// 避けるため)。
fn augment_page_with_versions(state: &AppState, page: PageResult) -> CatalogPageDto {
    struct RecordExtras {
        version: u64,
        path: String,
        mtime_ms: u64,
        mime_type: Option<String>,
        db_state: ItemDbState,
    }
    let mut extras: HashMap<AssetId, RecordExtras> = HashMap::with_capacity(page.items.len());
    {
        let inner = state.lock_catalog();
        let mode = inner.state.mode;
        let wanted: HashSet<AssetId> = page.items.iter().map(|item| item.id).collect();
        for record in inner.records() {
            if wanted.contains(&record.id) {
                extras.insert(
                    record.id,
                    RecordExtras {
                        version: asset_version(
                            &record.source_path,
                            record.byte_size,
                            record.mtime_ms,
                        ),
                        path: record.source_path.to_string_lossy().into_owned(),
                        mtime_ms: record.mtime_ms,
                        mime_type: record.mime_type.clone(),
                        db_state: ItemDbState::of(mode, record.id),
                    },
                );
            }
        }
    }
    CatalogPageDto {
        generation: page.generation,
        total: page.total,
        offset: page.offset,
        has_more: page.has_more,
        items: page
            .items
            .into_iter()
            .map(|item| {
                // レコード不在 (走査中の競合など) はversion 0・空pathで妥協する
                let e = extras.get(&item.id);
                CatalogItemDto {
                    version: e.map(|e| e.version).unwrap_or(0),
                    id: item.id,
                    display_name: item.display_name,
                    width: item.width,
                    height: item.height,
                    byte_size: item.byte_size,
                    state: item.state,
                    thumbnail_state: item.thumbnail_state,
                    is_animated: item.is_animated,
                    path: e.map(|e| e.path.clone()).unwrap_or_default(),
                    mtime_ms: e.map(|e| e.mtime_ms).unwrap_or(0),
                    mime_type: e.and_then(|e| e.mime_type.clone()),
                    db_state: e.map(|e| e.db_state).unwrap_or(ItemDbState::Adhoc),
                    triage: item.triage,
                    rating: item.rating,
                    favorite: item.favorite,
                }
            })
            .collect(),
    }
}

#[tauri::command]
pub fn get_catalog_page(
    state: State<'_, AppState>,
    generation: u64,
    offset: u32,
    limit: u32,
) -> Result<CatalogPageDto, PageError> {
    let page = lock(&state.catalog).state.page(generation, offset, limit)?;
    Ok(augment_page_with_versions(&state, page))
}

#[tauri::command]
pub fn start_scan(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let (generation, token) = {
        let mut inner = lock(&state.catalog);
        // 走査中の再投入も許可: begin_scanが旧トークンを無効化し、
        // 旧走査ワーカーの結果は破棄される (QA-WP4)
        begin_scan(&mut inner)
    };
    let dataset = state.current_folder();
    tracing::info!(
        dataset = %dataset.display(),
        generation,
        "scan started"
    );
    std::thread::spawn(move || run_scan(app, dataset, generation, token));
    Ok(())
}

/// 実行中の一時走査をキャンセルする (QA-WP4)。確定済みの部分結果は
/// そのまま表示に残る。登録ルート (ライブラリ) 走査のキャンセルは
/// 既存 `scan_stop` を使う。
#[tauri::command]
pub fn cancel_scan(app: AppHandle, state: State<'_, AppState>) -> Result<CatalogSummary, String> {
    let (summary, final_payload) = {
        let mut inner = lock(&state.catalog);
        let payload = cancel_scan_transition(&mut inner);
        (summary_of(&inner), payload)
    };
    if let Some(payload) = final_payload {
        tracing::info!(generation = summary.generation, "adhoc scan cancelled");
        let _ = app.emit("scan:progress", payload);
        let _ = app.emit(
            "catalog:changed",
            CatalogChangedPayload {
                generation: summary.generation,
                reason: CatalogChangeReason::ScanDone,
            },
        );
    }
    Ok(summary)
}

/// 閲覧フォルダを切り替えて一時走査を開始する (WP-4。ツリー選択からの流入)。
///
/// - 指定パスが既存ディレクトリでない場合はエラー (UIは状態表示へ反映)
/// - 走査中でも切替可能: 既存走査はトークン無効化で破棄され、新フォルダの
///   走査に置き換わる (旧結果は混入しない。QA-WP4)
/// - `sort` 指定時は走査開始時点のソートへ反映する (走査中は
///   `set_catalog_sort` を使えないため。QA-WP4 Major-2)
/// - 成功時: `dataset_path` を更新し `start_scan` と同じ走査を非同期投入する
///   (Realモードへ戻り、表示内容は新フォルダの走査結果で置き換わる)
#[tauri::command]
pub fn open_folder(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
    sort: Option<String>,
) -> Result<CatalogSummary, String> {
    let folder = validate_folder(Path::new(&path))?;
    let parsed_sort = match sort.as_deref() {
        None => None,
        Some(value) => Some(
            CatalogSort::from_str_opt(value)
                .ok_or_else(|| format!("不明なソートです: {value} (name|date_desc|size_desc)"))?,
        ),
    };
    let (summary, token) = {
        let mut inner = lock(&state.catalog);
        if let Some(parsed) = parsed_sort {
            inner.sort = parsed;
        }
        *state
            .dataset_path
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = folder.clone();
        let (_generation, token) = begin_scan(&mut inner);
        (summary_of(&inner), token)
    };
    tracing::info!(
        folder = %folder.display(),
        generation = summary.generation,
        "folder opened (adhoc scan started)"
    );
    std::thread::spawn(move || run_scan(app, folder, summary.generation, token));
    // begin_scan後・走査完了前のsummary (scanning=true) を返す
    Ok(summary)
}

/// 走査対象として妥当なフォルダか検証する。表示用のエラー文言を返す。
fn validate_folder(path: &Path) -> Result<PathBuf, String> {
    match std::fs::metadata(path) {
        Ok(meta) => {
            if meta.is_dir() {
                Ok(path.to_path_buf())
            } else {
                Err(format!("フォルダではありません: {}", path.display()))
            }
        }
        Err(err) => match err.kind() {
            std::io::ErrorKind::NotFound => {
                Err(format!("フォルダが見つかりません: {}", path.display()))
            }
            std::io::ErrorKind::PermissionDenied => {
                Err(format!("読み取り権限がありません: {}", path.display()))
            }
            _ => Err(format!(
                "フォルダにアクセスできません: {} ({err})",
                path.display()
            )),
        },
    }
}

/// ソート変更の状態遷移 (set_catalog_sort本体。走査中・コレクション表示中は拒否)。
pub(crate) fn apply_sort_change(
    inner: &mut CatalogInner,
    parsed: CatalogSort,
) -> Result<ModeSwitchOutcome, String> {
    if inner.state.scanning {
        return Err("走査中はソートを変更できません".to_string());
    }
    if inner.collection.is_some() {
        return Err(
            "コレクション表示中はソートを変更できません (並び順はコレクションが保持します)"
                .to_string(),
        );
    }
    if inner.sort == parsed {
        Ok(ModeSwitchOutcome::Unchanged(summary_of(inner)))
    } else {
        inner.sort = parsed;
        inner.apply_sort();
        inner.state.generation += 1;
        Ok(ModeSwitchOutcome::Changed(summary_of(inner)))
    }
}

/// 表示ソートを切り替える (WP-4)。recordsを並べ替え、generationを進めて
/// `catalog:changed` をemitする (ページ再取得で追従)。
/// コレクション表示中は拒否する (WP-6: manual=position順・smart=評価順が
/// 表示の正本であるため、ソートで壊せないようにする)。
#[tauri::command]
pub fn set_catalog_sort(
    app: AppHandle,
    state: State<'_, AppState>,
    sort: String,
) -> Result<CatalogSummary, String> {
    let parsed = CatalogSort::from_str_opt(&sort)
        .ok_or_else(|| format!("不明なソートです: {sort} (name|date_desc|size_desc)"))?;
    let outcome = {
        let mut inner = lock(&state.catalog);
        apply_sort_change(&mut inner, parsed)?
    };
    match outcome {
        ModeSwitchOutcome::Changed(summary) => {
            let _ = app.emit(
                "catalog:changed",
                CatalogChangedPayload {
                    generation: summary.generation,
                    reason: CatalogChangeReason::SortChange,
                },
            );
            Ok(summary)
        }
        ModeSwitchOutcome::Unchanged(summary) => Ok(summary),
    }
}

#[cfg(any(debug_assertions, feature = "devtools"))]
#[tauri::command]
pub fn set_catalog_mode(
    app: AppHandle,
    state: State<'_, AppState>,
    r#virtual: bool,
) -> Result<CatalogSummary, String> {
    let outcome = switch_mode(&mut lock(&state.catalog), r#virtual)?;
    match outcome {
        ModeSwitchOutcome::Changed(summary) => {
            let _ = app.emit(
                "catalog:changed",
                CatalogChangedPayload {
                    generation: summary.generation,
                    reason: CatalogChangeReason::ModeSwitch,
                },
            );
            Ok(summary)
        }
        ModeSwitchOutcome::Unchanged(summary) => Ok(summary),
    }
}

/// バックグラウンド走査スレッドの本体。UIスレッドで実行しないこと。
/// `token` はこの走査の世代。`open_folder` / `cancel_scan` で無効化された
/// 場合、以降のバッチは破棄され走査を中止する (QA-WP4)。
fn run_scan(app: AppHandle, dataset: PathBuf, generation: u64, token: u64) {
    let started = Instant::now();
    // プローブ1件は1ms未満のため、Duration累積でないとms換算時に0に落ちる
    let mut probe_elapsed = std::time::Duration::ZERO;
    let mut next_id: u64 = 1;

    let sink = &mut |batch: ScanBatch| -> bool {
        let mut records = Vec::with_capacity(batch.files.len());
        for file in &batch.files {
            let probe_started = Instant::now();
            let probe = probe_image(&file.path);
            probe_elapsed += probe_started.elapsed();
            // magicが正本。AVIFもavif-native (dav1d) でプローブされる
            // (AVIF-1で有効化。handoff参照)
            let (state, width, height, mime, is_animated) = match probe {
                ImageProbe::Ok(info) => (
                    ImageState::Ready,
                    Some(info.width),
                    Some(info.height),
                    Some(info.format.mime().to_string()),
                    info.is_animated,
                ),
                ImageProbe::Unreadable => (ImageState::Unreadable, None, None, None, false),
                ImageProbe::Corrupt => (ImageState::Corrupt, None, None, None, false),
                ImageProbe::Unsupported => (ImageState::Unsupported, None, None, None, false),
            };
            records.push(ImageRecord {
                id: AssetId::new(next_id),
                source_path: file.path.clone(),
                display_name: file
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| file.path.to_string_lossy().into_owned()),
                width,
                height,
                byte_size: file.size,
                mtime_ms: file.mtime_ms,
                mime_type: mime,
                is_animated,
                state,
                thumb_state: ThumbnailState::Missing,
                rating: 0,
                triage: Triage::Undecided,
                favorite: false,
            });
            next_id += 1;
        }
        let probe_failures: Vec<String> = records
            .iter()
            .filter(|r| r.state != ImageState::Ready)
            .map(|r| format!("{}: {:?}", r.source_path.display(), r.state))
            .collect();

        let payload = {
            let state = app.state::<AppState>();
            let mut inner = lock(&state.catalog);
            if !apply_scan_batch(
                &mut inner,
                token,
                records,
                batch.entries_walked,
                &probe_failures,
            ) {
                // 新しい走査 (フォルダ切替・再投入) が支配中。結果を捨てて中止。
                return false;
            }
            ScanProgressPayload {
                scanned: inner.progress.scanned,
                found: inner.progress.found,
                errors: inner.progress.errors,
                elapsed_ms: started.elapsed().as_millis() as u64,
                done: false,
                error_samples: inner.progress.error_samples.clone(),
                updated: 0,
                relocated: 0,
                missing: 0,
                hashed: 0,
                pending_hash: 0,
                phase: None,
            }
        };
        let _ = app.emit("scan:progress", payload);
        let _ = app.emit(
            "catalog:changed",
            CatalogChangedPayload {
                generation,
                reason: CatalogChangeReason::ScanBatch,
            },
        );
        true
    };

    let result = scan_images_batch(&dataset, sink);
    let scan_elapsed_ms = started.elapsed().as_millis() as u64;
    let rss_kb = read_self_rss_kb();

    let final_payload = {
        let state = app.state::<AppState>();
        let mut inner = lock(&state.catalog);
        finish_scan(
            &mut inner,
            token,
            scan_elapsed_ms,
            result.entries,
            &result.errors,
            result.truncated,
        )
    };

    match final_payload {
        Some(payload) => {
            // 完了した走査のみ計測を出す (計測ドライバは "scan metrics" を
            // 走査完了の合図にgrepするため、破棄分では出力しない)
            log_scan_metrics(&ScanMetrics {
                root: &dataset,
                found: result.png_count,
                errors: result.errors.len() as u64,
                scan_elapsed_ms: result.elapsed_ms,
                probe_elapsed_ms: probe_elapsed.as_millis() as u64,
                rss_kb,
            });
            let _ = app.emit("scan:progress", payload);
            let _ = app.emit(
                "catalog:changed",
                CatalogChangedPayload {
                    generation,
                    reason: CatalogChangeReason::ScanDone,
                },
            );
            tracing::info!(generation, "scan thread finished");
        }
        None => {
            tracing::info!(generation, "scan abandoned (superseded by newer scan)");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(path: &str) -> ImageRecord {
        ImageRecord {
            id: AssetId::new(1),
            source_path: PathBuf::from(path),
            display_name: path.to_string(),
            width: Some(10),
            height: Some(20),
            byte_size: 100,
            mtime_ms: 1,
            mime_type: None,
            is_animated: false,
            state: ImageState::Ready,
            thumb_state: ThumbnailState::Missing,
            rating: 0,
            triage: Triage::Undecided,
            favorite: false,
        }
    }

    #[test]
    fn begin_scan_resets_and_bumps_generation() {
        let mut inner = CatalogInner::new();
        let gen0 = inner.state.generation;
        inner.state.records.push(record("a.png"));
        let (gen1, token1) = begin_scan(&mut inner);
        assert_eq!(gen1, gen0 + 1);
        assert!(inner.state.scanning);
        assert!(inner.state.records.is_empty());
        assert_eq!(inner.state.mode, CatalogMode::Real);
        assert_eq!(inner.progress.found, 0);
        assert!(inner.scan_started_at.is_some());
        assert!(inner.last_scan_elapsed_ms.is_none());

        // 再投入は旧トークンを無効化する (走査中のフォルダ切替。QA-WP4)
        let (gen2, token2) = begin_scan(&mut inner);
        assert_eq!(gen2, gen0 + 2);
        assert_ne!(token1, token2);
    }

    // ===== 走査トークン (世代切替・キャンセル。QA-WP4 Major-1) =====

    #[test]
    fn apply_scan_batch_discards_stale_token() {
        let mut inner = CatalogInner::new();
        let (_, token) = begin_scan(&mut inner);
        assert!(apply_scan_batch(
            &mut inner,
            token,
            vec![record("a.png")],
            1,
            &[]
        ));
        assert_eq!(inner.state.records.len(), 1);
        assert_eq!(inner.progress.found, 1);

        // 新規走査の開始 (open_folderでの切替) 後、旧トークンのバッチは破棄
        begin_scan(&mut inner);
        assert!(!apply_scan_batch(
            &mut inner,
            token,
            vec![record("b.png")],
            2,
            &[]
        ));
        assert_eq!(
            inner.state.records.len(),
            0,
            "新走査のカタログへ旧結果が混入しない"
        );
        assert_eq!(inner.progress.found, 0);
    }

    #[test]
    fn finish_scan_ignores_stale_token_and_keeps_new_scan_state() {
        let mut inner = CatalogInner::new();
        let (_, old_token) = begin_scan(&mut inner);
        let (_, new_token) = begin_scan(&mut inner);
        assert!(finish_scan(&mut inner, old_token, 10, 5, &[], false).is_none());
        assert!(
            inner.state.scanning,
            "新走査の進行状態を旧ワーカーの完了確定が壊さない"
        );
        assert!(finish_scan(&mut inner, new_token, 10, 5, &[], false).is_some());
        assert!(!inner.state.scanning);
    }

    #[test]
    fn finish_scan_reports_truncation_warning() {
        let mut inner = CatalogInner::new();
        let (_, token) = begin_scan(&mut inner);
        let payload = finish_scan(&mut inner, token, 25, 7, &[], true).unwrap();
        assert!(payload.done);
        assert!(!inner.state.scanning);
        assert!(
            inner
                .progress
                .error_samples
                .iter()
                .any(|m| m.contains("打ち切りました")),
            "件数上限到達は結果 (エラーサンプル) へ警告反映される"
        );
        assert_eq!(inner.last_scan_elapsed_ms, Some(25));
    }

    #[test]
    fn cancel_scan_transition_stops_and_keeps_partial_records() {
        let mut inner = CatalogInner::new();
        let (_, token) = begin_scan(&mut inner);
        apply_scan_batch(
            &mut inner,
            token,
            vec![record("a.png"), record("b.png")],
            2,
            &[],
        );
        let payload = cancel_scan_transition(&mut inner).unwrap();
        assert!(payload.done);
        assert!(!inner.state.scanning);
        assert_eq!(
            inner.state.records.len(),
            2,
            "確定済みの部分結果は表示に残す"
        );
        assert_ne!(inner.scan_token, token, "旧走査ワーカーは無効化される");
        // 旧ワーカーの完了確定は無視される
        assert!(finish_scan(&mut inner, token, 99, 9, &[], false).is_none());
        // 非走査時のキャンセルは何もしない
        assert!(cancel_scan_transition(&mut inner).is_none());
    }

    // ===== モード切替 (devtools機能。debug/devtoolsビルドのみ) =====

    #[cfg(any(debug_assertions, feature = "devtools"))]
    #[test]
    fn switch_mode_rejects_while_scanning() {
        let mut inner = CatalogInner::new();
        begin_scan(&mut inner);
        assert!(switch_mode(&mut inner, true).is_err());
        assert!(switch_mode(&mut inner, false).is_err());
    }

    #[cfg(any(debug_assertions, feature = "devtools"))]
    #[test]
    fn switch_to_virtual_requires_real_records() {
        let mut inner = CatalogInner::new();
        let err = switch_mode(&mut inner, true).unwrap_err();
        assert!(err.contains("スキャンを実行"));
    }

    #[cfg(any(debug_assertions, feature = "devtools"))]
    #[test]
    fn switch_mode_virtual_and_back() {
        let mut inner = CatalogInner::new();
        inner.real = vec![record("a.png"), record("b.png")];
        inner.state.records = inner.real.clone();

        let gen_before = inner.state.generation;
        match switch_mode(&mut inner, true).unwrap() {
            ModeSwitchOutcome::Changed(summary) => {
                assert_eq!(summary.mode, CatalogMode::Virtual);
                assert_eq!(summary.total as usize, VIRTUAL_CATALOG_COUNT);
                assert_eq!(summary.generation, gen_before + 1);
            }
            ModeSwitchOutcome::Unchanged(_) => panic!("expected change"),
        }
        assert_eq!(inner.state.records.len(), VIRTUAL_CATALOG_COUNT);
        assert_eq!(inner.state.records[0].display_name, "virtual-00000-a.png");

        // 同一モードへの再切替は変化なし
        let gen_now = inner.state.generation;
        match switch_mode(&mut inner, true).unwrap() {
            ModeSwitchOutcome::Unchanged(summary) => {
                assert_eq!(summary.generation, gen_now);
            }
            ModeSwitchOutcome::Changed(_) => panic!("expected unchanged"),
        }

        // 実へ戻す
        match switch_mode(&mut inner, false).unwrap() {
            ModeSwitchOutcome::Changed(summary) => {
                assert_eq!(summary.mode, CatalogMode::Real);
                assert_eq!(summary.total, 2);
            }
            ModeSwitchOutcome::Unchanged(_) => panic!("expected change"),
        }
        assert_eq!(inner.state.records.len(), 2);
    }

    // ===== コレクション表示とソース切替 (WP-6) =====

    #[test]
    fn source_switch_from_collection_view_restores_library_records() {
        // コレクション表示中はmode=Libraryのままstate.recordsがビューを
        // 持つ。同一モードへの切替でもlibrary recordsへ復帰しなければ
        // ならない (WP-6実機E2Eで発見した回帰)。
        let mut inner = CatalogInner::new();
        let lib = vec![record("a.png"), record("b.png"), record("c.png")];
        inner.library = lib.clone();
        inner.state.records = lib.clone();
        inner.state.mode = CatalogMode::Library;
        // コレクション表示 (1件のみ) へ切替
        let gen0 = inner.state.generation;
        let gen1 = inner.set_collection_display(
            CollectionView { collection_id: 7 },
            vec![record("only.png")],
        );
        assert_eq!(gen1, gen0 + 1);
        assert_eq!(inner.state.records.len(), 1);
        assert_eq!(
            inner.collection_view(),
            Some(CollectionView { collection_id: 7 })
        );
        // libraryソースへの切替 (同一モード) は「変化なし」ではなく復帰
        let gen2 = inner.state.generation;
        match switch_catalog_source(&mut inner, CatalogMode::Library) {
            ModeSwitchOutcome::Changed(summary) => {
                assert_eq!(summary.total, 3, "libraryの全件が復帰する");
                assert_eq!(summary.generation, gen2 + 1);
            }
            ModeSwitchOutcome::Unchanged(_) => panic!("collection view must not be unchanged"),
        }
        assert_eq!(inner.state.records.len(), 3);
        assert!(inner.collection_view().is_none());
        // 通常の同一モード切替は変化なし (generation不変)
        let gen3 = inner.state.generation;
        assert!(matches!(
            switch_catalog_source(&mut inner, CatalogMode::Library),
            ModeSwitchOutcome::Unchanged(_)
        ));
        assert_eq!(inner.state.generation, gen3);
        // コレクション表示中のtemp切替も復帰経路 (Realモード差し替え)
        inner.set_collection_display(CollectionView { collection_id: 9 }, vec![record("x.png")]);
        match switch_catalog_source(&mut inner, CatalogMode::Real) {
            ModeSwitchOutcome::Changed(summary) => assert_eq!(summary.mode, CatalogMode::Real),
            ModeSwitchOutcome::Unchanged(_) => panic!("expected change to real"),
        }
        assert!(inner.collection_view().is_none());
    }

    #[test]
    fn summary_reflects_state_and_progress() {
        let mut inner = CatalogInner::new();
        let summary = summary_of(&inner);
        assert_eq!(summary.generation, 1);
        assert_eq!(summary.mode, CatalogMode::Real);
        assert_eq!(summary.total, 0);
        assert!(!summary.scanning);
        assert_eq!(summary.scanned_count, 0);
        assert_eq!(summary.error_count, 0);
        assert_eq!(summary.last_scan_elapsed_ms, None);

        begin_scan(&mut inner);
        inner.progress.scanned = 12;
        inner.progress.found = 10;
        inner.progress.push_error("x.png: Corrupt".to_string());
        inner.state.records = vec![record("a.png")];
        let scanning = summary_of(&inner);
        assert!(scanning.scanning);
        assert_eq!(scanning.scanned_count, 12);
        assert_eq!(scanning.error_count, 1);
        assert!(scanning.last_scan_elapsed_ms.is_some());
    }

    #[test]
    fn error_samples_are_bounded() {
        let mut progress = ProgressCounters::default();
        for i in 0..10 {
            progress.push_error(format!("e{i}"));
        }
        assert_eq!(progress.errors, 10);
        assert_eq!(progress.error_samples.len(), ERROR_SAMPLE_LIMIT);
        assert_eq!(progress.error_samples[0], "e5");
        assert_eq!(progress.error_samples[4], "e9");
    }

    // ===== ページDTOの内容感応version (FG-1 / F3) =====

    fn record_with(path: &str, id: u64, byte_size: u64, mtime_ms: u64) -> ImageRecord {
        ImageRecord {
            id: AssetId::new(id),
            source_path: PathBuf::from(path),
            display_name: path.rsplit('/').next().unwrap_or(path).to_string(),
            width: Some(100),
            height: Some(200),
            byte_size,
            mtime_ms,
            mime_type: None,
            is_animated: false,
            state: ImageState::Ready,
            thumb_state: ThumbnailState::Missing,
            rating: 0,
            triage: Triage::Undecided,
            favorite: false,
        }
    }

    #[test]
    fn page_dto_items_carry_version_that_tracks_content() {
        let state = AppState::new(PathBuf::from("/tmp/gento-test-dataset"));
        {
            let mut inner = state.lock_catalog();
            inner
                .state
                .records
                .push(record_with("/x/a.png", 1, 100, 1_000));
        }
        // page()とaugment()は別statementで (augment内でロックを取り直すため)
        let page = state.lock_catalog().state.page(1, 0, 10).expect("page");
        let dto = augment_page_with_versions(&state, page);
        assert_eq!(dto.items.len(), 1);
        let v1 = dto.items[0].version;
        assert!(v1 > 0);

        // 元ファイルのsize変化 → version変化 (同一assetIdのまま)
        state.lock_catalog().state.records[0].byte_size = 101;
        let page = state.lock_catalog().state.page(1, 0, 10).expect("page");
        let dto2 = augment_page_with_versions(&state, page);
        assert_ne!(dto2.items[0].version, v1);

        // mtime変化でも変化
        state.lock_catalog().state.records[0].mtime_ms = 2_000;
        let page = state.lock_catalog().state.page(1, 0, 10).expect("page");
        let dto3 = augment_page_with_versions(&state, page);
        assert_ne!(dto3.items[0].version, v1);
        assert_ne!(dto3.items[0].version, dto2.items[0].version);
        // クエリ桁上限 (18桁) に収まる
        assert!(dto3.items[0].version.to_string().len() <= 18);
    }

    #[test]
    fn page_dto_serialization_shape_keeps_camel_case_and_adds_version() {
        let state = AppState::new(PathBuf::from("/tmp/gento-test-dataset"));
        {
            let mut inner = state.lock_catalog();
            let mut rec = record_with("/x/a.png", 42, 1234, 5);
            rec.mime_type = Some("image/png".to_string());
            inner.state.records.push(rec);
        }
        let page = state.lock_catalog().state.page(1, 0, 10).expect("page");
        let dto = augment_page_with_versions(&state, page);
        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(
            json["items"][0],
            serde_json::json!({
                "id": "42",
                "displayName": "a.png",
                "width": 100,
                "height": 200,
                "byteSize": 1234,
                "state": "ready",
                "thumbnailState": "missing",
                "isAnimated": false,
                "version": dto.items[0].version,
                // WP-4: 情報パネル用フィールド
                "path": "/x/a.png",
                "mtimeMs": 5,
                "mimeType": "image/png",
                "dbState": "adhoc", // Real (一時走査) モード
                "triage": "undecided",
                // WP-5: グリッドバッジ表示用フィールド
                "rating": 0,
                "favorite": false,
            })
        );
    }

    // ===== 情報パネル用DTO (WP-4) =====

    #[test]
    fn db_state_reflects_mode_and_asset_id_encoding() {
        // Library + 平素なID = Asset確定
        let state = AppState::new(PathBuf::from("/tmp/gento-test-dataset"));
        {
            let mut inner = state.lock_catalog();
            inner.state.mode = CatalogMode::Library;
            inner.state.records = vec![record_with("/r/a.png", 7, 1, 1)];
        }
        let page = state.lock_catalog().state.page(1, 0, 10).expect("page");
        let dto = augment_page_with_versions(&state, page);
        assert_eq!(dto.items[0].db_state, ItemDbState::Asset);
        assert_eq!(
            serde_json::to_value(dto.items[0].db_state).unwrap(),
            serde_json::json!("asset")
        );

        // Library + bit63仮ID = Provisional (ハッシュ待ち)
        {
            let mut inner = state.lock_catalog();
            inner.state.records = vec![record_with("/r/b.png", 1 << 63 | 9, 1, 1)];
        }
        let page = state.lock_catalog().state.page(1, 0, 10).expect("page");
        let dto = augment_page_with_versions(&state, page);
        assert_eq!(dto.items[0].db_state, ItemDbState::Provisional);
        assert_eq!(
            serde_json::to_value(dto.items[0].db_state).unwrap(),
            serde_json::json!("provisional")
        );

        // Real = Adhoc (IDエンコードに依らない)
        {
            let mut inner = state.lock_catalog();
            inner.state.mode = CatalogMode::Real;
            inner.state.records = vec![record_with("/r/c.png", 1 << 63 | 9, 1, 1)];
        }
        let page = state.lock_catalog().state.page(1, 0, 10).expect("page");
        let dto = augment_page_with_versions(&state, page);
        assert_eq!(dto.items[0].db_state, ItemDbState::Adhoc);
    }

    // ===== ソート (WP-4) =====

    #[test]
    fn set_catalog_sort_logic_reorders_and_bumps_generation() {
        let mut inner = CatalogInner::new();
        inner.state.records = vec![
            record_with("/x/a.png", 1, 10, 100),
            record_with("/x/b.png", 2, 900, 50),
        ];
        inner.apply_sort();
        let names: Vec<&str> = inner
            .state
            .records
            .iter()
            .map(|r| r.display_name.as_str())
            .collect();
        assert_eq!(names, vec!["a.png", "b.png"], "既定は名前順");

        inner.sort = CatalogSort::SizeDesc;
        inner.apply_sort();
        let names: Vec<&str> = inner
            .state
            .records
            .iter()
            .map(|r| r.display_name.as_str())
            .collect();
        assert_eq!(names, vec!["b.png", "a.png"]);
    }

    #[test]
    fn switch_source_applies_current_sort() {
        let mut inner = CatalogInner::new();
        inner.real = vec![record_with("/x/a.png", 1, 10, 100)];
        inner.library = vec![
            record_with("/r/z.png", 2, 10, 900),
            record_with("/r/y.png", 3, 10, 100),
        ];
        inner.sort = CatalogSort::DateDesc;
        inner.state.records = inner.real.clone();
        inner.state.mode = CatalogMode::Real;
        // set_catalog_sourceと同じ swap→sort→generation の流れ
        inner.state.records = inner.library.clone();
        inner.state.mode = CatalogMode::Library;
        inner.apply_sort();
        let names: Vec<&str> = inner
            .state
            .records
            .iter()
            .map(|r| r.display_name.as_str())
            .collect();
        assert_eq!(names, vec!["z.png", "y.png"], "mtime降順が適用される");
    }

    // ===== open_folderの検証 (WP-4) =====

    #[test]
    fn validate_folder_classifies_errors() {
        let dir = std::env::temp_dir().join(format!("gento-openfolder-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(validate_folder(&dir), Ok(dir.clone()));
        assert!(validate_folder(&dir.join("gone"))
            .unwrap_err()
            .contains("見つかりません"));
        let file = dir.join("f.txt");
        std::fs::write(&file, b"x").unwrap();
        assert!(validate_folder(&file)
            .unwrap_err()
            .contains("フォルダではありません"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sort_change_reason_serializes() {
        assert_eq!(
            serde_json::to_value(CatalogChangeReason::SortChange).unwrap(),
            serde_json::json!("sort_change")
        );
    }

    #[test]
    fn payload_serialization_shapes() {
        let progress = ScanProgressPayload {
            scanned: 5,
            found: 4,
            errors: 1,
            elapsed_ms: 123,
            done: false,
            error_samples: vec!["a.png: Corrupt".to_string()],
            updated: 0,
            relocated: 0,
            missing: 0,
            hashed: 0,
            pending_hash: 0,
            phase: None,
        };
        assert_eq!(
            serde_json::to_value(&progress).unwrap(),
            serde_json::json!({
                "scanned": 5,
                "found": 4,
                "errors": 1,
                "elapsed_ms": 123,
                "done": false,
                "error_samples": ["a.png: Corrupt"],
                // WP-2ライブラリスキャンフィールド (一時走査では初期値)
                "updated": 0,
                "relocated": 0,
                "missing": 0,
                "hashed": 0,
                "pending_hash": 0,
                "phase": null,
            })
        );

        let changed = CatalogChangedPayload {
            generation: 3,
            reason: CatalogChangeReason::ScanBatch,
        };
        assert_eq!(
            serde_json::to_value(changed).unwrap(),
            serde_json::json!({"generation": 3, "reason": "scan_batch"})
        );

        let summary = CatalogSummary {
            generation: 2,
            mode: CatalogMode::Virtual,
            total: 50_000,
            scanning: false,
            scanned_count: 1615,
            error_count: 0,
            last_scan_elapsed_ms: Some(999),
            sort: CatalogSort::DateDesc,
        };
        assert_eq!(
            serde_json::to_value(&summary).unwrap(),
            serde_json::json!({
                "generation": 2,
                "mode": "virtual",
                "total": 50000,
                "scanning": false,
                "scanned_count": 1615,
                "error_count": 0,
                "last_scan_elapsed_ms": 999,
                "sort": "date_desc",
            })
        );
    }
}
