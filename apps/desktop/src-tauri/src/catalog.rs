//! カタログcommand群とバックグラウンド走査 (Phase 2 / WP-2)。
//!
//! 本モジュールはthin adapter: ビジネスロジックはgento-core / gento-imageにある。
//! UIスレッドでファイルI/Oを行わない (走査・プローブは専用スレッド)。

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use gento_core::catalog::{
    build_virtual, CatalogMode, CatalogState, ImageRecord, ImageState, PageError, PageResult,
    ThumbnailState, Triage, VIRTUAL_CATALOG_COUNT,
};
use gento_core::AssetId;
use gento_image::metrics::{log_scan_metrics, read_self_rss_kb, ScanMetrics};
use gento_image::probe::{probe_png_dimensions, ProbeResult};
use gento_image::scan::{scan_pngs, ScanBatch};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

/// 進捗イベントに載せる直近エラーサンプルの上限 (イベント嵐対策)。
const ERROR_SAMPLE_LIMIT: usize = 5;

/// Tauri command間で共有する実行時状態。
pub struct AppState {
    pub dataset_path: PathBuf,
    pub catalog: Mutex<CatalogInner>,
    /// サムネイル生成サービス (WP-3)。setupで生成、Exitでshutdown。
    pub thumbnails: Option<Arc<crate::thumbnails::ThumbnailService>>,
    /// ローカル画像プロトコルの統計 (WP-4)。
    pub proto_stats: Arc<crate::protocol::ProtoStats>,
}

/// ロック内で保持するカタログ実状態。
pub struct CatalogInner {
    /// 稼働モードに応じた「表示中」レコード列。
    state: CatalogState,
    /// 最後に完了した走査の実レコード (仮想カタログのソース)。
    real: Vec<ImageRecord>,
    progress: ProgressCounters,
    scan_started_at: Option<Instant>,
    last_scan_elapsed_ms: Option<u64>,
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
            progress: ProgressCounters::default(),
            scan_started_at: None,
            last_scan_elapsed_ms: None,
        }
    }
}

impl AppState {
    pub fn new(dataset_path: PathBuf) -> Self {
        Self {
            dataset_path,
            catalog: Mutex::new(CatalogInner::new()),
            thumbnails: None,
            proto_stats: Arc::new(crate::protocol::ProtoStats::new()),
        }
    }

    /// カタログロックの取得 (poisoning復旧付き)。
    pub fn lock_catalog(&self) -> MutexGuard<'_, CatalogInner> {
        lock(&self.catalog)
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
    /// IDからレコードを引く (線形探索。コマンド単位の規模想定)。
    pub fn record(&self, id: AssetId) -> Option<&ImageRecord> {
        self.state.records.iter().find(|r| r.id == id)
    }

    /// 表示中カタログのレコード列 (スナップショット抽出用)。
    pub fn records(&self) -> &[ImageRecord] {
        &self.state.records
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
}

/// `scan:progress` イベントペイロード。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScanProgressPayload {
    pub scanned: u64,
    pub found: u64,
    pub errors: u64,
    pub elapsed_ms: u64,
    pub done: bool,
    /// 直近数件のエラーサンプル (毎件は流さない)。
    pub error_samples: Vec<String>,
}

/// `catalog:changed` イベントの理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogChangeReason {
    ScanBatch,
    ScanDone,
    ModeSwitch,
}

/// `catalog:changed` イベントペイロード。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CatalogChangedPayload {
    pub generation: u64,
    pub reason: CatalogChangeReason,
}

/// poisoning時も内容を復旧扱いにする (走査スレッドpanicでUIが固まらないよう)。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
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
    }
}

/// 走査開始の状態遷移。呼び出し前に `scanning == false` を確認すること。
fn begin_scan(inner: &mut CatalogInner) -> u64 {
    inner.state.scanning = true;
    // 再走査はカタログを置き換えるためRealモードへ戻し、内容をクリアする
    inner.state.records.clear();
    inner.state.mode = CatalogMode::Real;
    inner.state.generation += 1;
    inner.progress = ProgressCounters::default();
    inner.scan_started_at = Some(Instant::now());
    inner.last_scan_elapsed_ms = None;
    inner.state.generation
}

#[derive(Debug)]
enum ModeSwitchOutcome {
    Changed(CatalogSummary),
    /// 同一モードへの切替は何も変えない (generation不変)。
    Unchanged(CatalogSummary),
}

fn switch_mode(inner: &mut CatalogInner, want_virtual: bool) -> Result<ModeSwitchOutcome, String> {
    if inner.state.scanning {
        return Err("cannot switch catalog mode while scanning".to_string());
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
                "real catalog is empty; run start_scan before switching to virtual".to_string(),
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

#[tauri::command]
pub fn get_catalog_summary(state: State<'_, AppState>) -> CatalogSummary {
    summary_of(&lock(&state.catalog))
}

#[tauri::command]
pub fn get_catalog_page(
    state: State<'_, AppState>,
    generation: u64,
    offset: u32,
    limit: u32,
) -> Result<PageResult, PageError> {
    lock(&state.catalog).state.page(generation, offset, limit)
}

#[tauri::command]
pub fn start_scan(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let generation = {
        let mut inner = lock(&state.catalog);
        if inner.state.scanning {
            return Err("scan already in progress".to_string());
        }
        begin_scan(&mut inner)
    };
    tracing::info!(
        dataset = %state.dataset_path.display(),
        generation,
        "scan started"
    );
    let dataset = state.dataset_path.clone();
    std::thread::spawn(move || run_scan(app, dataset, generation));
    Ok(())
}

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
fn run_scan(app: AppHandle, dataset: PathBuf, generation: u64) {
    let started = Instant::now();
    // プローブ1件は1ms未満のため、Duration累積でないとms換算時に0に落ちる
    let mut probe_elapsed = std::time::Duration::ZERO;
    let mut next_id: u64 = 1;

    let sink = &mut |batch: ScanBatch| {
        let mut records = Vec::with_capacity(batch.files.len());
        for file in &batch.files {
            let probe_started = Instant::now();
            let probe = probe_png_dimensions(&file.path);
            probe_elapsed += probe_started.elapsed();
            let (state, width, height) = match probe {
                ProbeResult::Ok { width, height } => (ImageState::Ready, Some(width), Some(height)),
                ProbeResult::Unreadable => (ImageState::Unreadable, None, None),
                ProbeResult::Corrupt => (ImageState::Corrupt, None, None),
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
                state,
                thumb_state: ThumbnailState::Missing,
                rating: 0,
                triage: Triage::Undecided,
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
            inner.progress.scanned = batch.entries_walked;
            inner.progress.found += batch.files.len() as u64;
            for failure in &probe_failures {
                inner.progress.push_error(failure.clone());
            }
            inner.state.records.extend(records);
            inner.state.sort_records();
            ScanProgressPayload {
                scanned: inner.progress.scanned,
                found: inner.progress.found,
                errors: inner.progress.errors,
                elapsed_ms: started.elapsed().as_millis() as u64,
                done: false,
                error_samples: inner.progress.error_samples.clone(),
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
    };

    let result = scan_pngs(&dataset, sink);
    let scan_elapsed_ms = started.elapsed().as_millis() as u64;
    let rss_kb = read_self_rss_kb();

    let final_payload = {
        let state = app.state::<AppState>();
        let mut inner = lock(&state.catalog);
        inner.state.scanning = false;
        inner.progress.scanned = result.entries;
        for error in &result.errors {
            inner
                .progress
                .push_error(format!("{}: {}", error.path.display(), error.reason));
        }
        inner.last_scan_elapsed_ms = Some(scan_elapsed_ms);
        inner.real = inner.state.records.clone();
        ScanProgressPayload {
            scanned: inner.progress.scanned,
            found: inner.progress.found,
            errors: inner.progress.errors,
            elapsed_ms: scan_elapsed_ms,
            done: true,
            error_samples: inner.progress.error_samples.clone(),
        }
    };

    log_scan_metrics(&ScanMetrics {
        root: &dataset,
        found: final_payload.found,
        errors: final_payload.errors,
        scan_elapsed_ms: result.elapsed_ms,
        probe_elapsed_ms: probe_elapsed.as_millis() as u64,
        rss_kb,
    });
    let _ = app.emit("scan:progress", final_payload);
    let _ = app.emit(
        "catalog:changed",
        CatalogChangedPayload {
            generation,
            reason: CatalogChangeReason::ScanDone,
        },
    );
    tracing::info!(generation, "scan thread finished");
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
            state: ImageState::Ready,
            thumb_state: ThumbnailState::Missing,
            rating: 0,
            triage: Triage::Undecided,
        }
    }

    #[test]
    fn begin_scan_resets_and_bumps_generation() {
        let mut inner = CatalogInner::new();
        let gen0 = inner.state.generation;
        inner.state.records.push(record("a.png"));
        let gen1 = begin_scan(&mut inner);
        assert_eq!(gen1, gen0 + 1);
        assert!(inner.state.scanning);
        assert!(inner.state.records.is_empty());
        assert_eq!(inner.state.mode, CatalogMode::Real);
        assert_eq!(inner.progress.found, 0);
        assert!(inner.scan_started_at.is_some());
        assert!(inner.last_scan_elapsed_ms.is_none());
    }

    #[test]
    fn switch_mode_rejects_while_scanning() {
        let mut inner = CatalogInner::new();
        begin_scan(&mut inner);
        assert!(switch_mode(&mut inner, true).is_err());
        assert!(switch_mode(&mut inner, false).is_err());
    }

    #[test]
    fn switch_to_virtual_requires_real_records() {
        let mut inner = CatalogInner::new();
        let err = switch_mode(&mut inner, true).unwrap_err();
        assert!(err.contains("run start_scan"));
    }

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

    #[test]
    fn payload_serialization_shapes() {
        let progress = ScanProgressPayload {
            scanned: 5,
            found: 4,
            errors: 1,
            elapsed_ms: 123,
            done: false,
            error_samples: vec!["a.png: Corrupt".to_string()],
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
            })
        );
    }
}
