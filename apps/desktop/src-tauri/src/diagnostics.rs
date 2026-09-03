//! 診断 (MVP WP-15): 5領域スナップショットと診断bundle。
//!
//! - スナップショットは**全てread-only** (quick_checkだけはFTS5整合性検査の
//!   内部書き込みが必要なため例外的にwriter接続で実行 — `Db::diagnose`
//!   と同一理由)。UI表示には実パスを含む (情報はローカル完結)。
//! - **bundle**はユーザー指定dirへJSON 1ファイルをtmp書き→sync→原子rename
//!   で出版する。内容から**APIキー・GPS・実パスを完全に除外**する:
//!   - APIキー: 設定・診断のいずれの項目にも鍵素材は含まない (構造的排除。
//!     SecretStore参照文字列も出さない)。全文字列は
//!     [`gento_core::redact::scrub_text`] を通す (迂回経路の最終防護)。
//!   - GPS: 診断項目にメタデータ列を出さない (件数・状態のみ)。
//!   - 実パス: 既知ディレクトリ (root/data/cache/log/thumbnails/models/db)
//!     をプレースホルダ (`<root:1>` 等) へ置換。homeも最終的に `<home>` へ。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gento_core::repo::LibraryRootRepo;
use gento_core::GENTO_API_VERSION;
use gento_db::Db;
use serde::Serialize;
use serde_json::{json, Value};
use tauri::State;

use crate::catalog::AppState;

/// bundle形式のversion。
pub const BUNDLE_FORMAT_VERSION: u32 = 1;
/// bundleへ含めるログ末尾の行数。
const BUNDLE_LOG_TAIL_LINES: usize = 100;

// ===== DTO (camelCase) =====

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DbAreaDto {
    pub db_open: bool,
    /// `PRAGMA quick_check` の要約 ("ok" または問題行)。
    pub integrity_check: String,
    pub schema_version: i64,
    pub db_bytes: u64,
    pub wal_bytes: u64,
    /// 設定の破損検知数 (既定値へフォールバックした件数)。
    pub settings_corrupt_count: u32,
    pub db_path: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FtsAreaDto {
    pub expected_docs: u64,
    pub actual_docs: u64,
    pub missing_docs: u64,
    pub orphan_docs: u64,
    pub duplicate_docs: u64,
    pub stale_contents: u64,
    pub consistent: bool,
    /// 最終全再構築時刻 (RFC3339。未実施 = null)。
    pub last_rebuild_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsearchAreaDto {
    pub model: String,
    pub indexed_count: u64,
    /// SQLite embeddings表の正本件数。
    pub sqlite_count: u64,
    /// indexed != sqlite (再構築が必要)。
    pub stale: bool,
    pub dimensions: Option<u32>,
    pub ready: bool,
    pub building: bool,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelPackageDto {
    pub model_id: String,
    /// "verified" | "missing" | "missing_files" | "mismatch"
    pub state: String,
    pub embedding_dim: u32,
    pub license: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelsAreaDto {
    pub enabled: bool,
    pub packages: Vec<ModelPackageDto>,
    /// "not_started" | "running" | "restarting" | "crashed"
    pub worker_phase: String,
    pub worker_restarts: u64,
    pub worker_last_error: Option<String>,
    pub loaded_model: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct JobCountDto {
    pub job_type: String,
    pub status: String,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct JobsAreaDto {
    pub by_type: Vec<JobCountDto>,
    /// 最古の待機中 (queued/retry_waiting) ジョブの経過秒数。無ければnull。
    pub oldest_pending_age_seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsDto {
    pub db: DbAreaDto,
    pub fts: FtsAreaDto,
    pub usearch: UsearchAreaDto,
    pub models: ModelsAreaDto,
    pub jobs: JobsAreaDto,
}

/// `diagnostics_bundle_create` の応答。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BundleResultDto {
    pub path: String,
}

// ===== 収集 =====

/// 診断に必要なAppStateの部分コピー (spawn_blockingへmoveするため。
/// State<'_, AppState> は関数外へ出せない)。
#[derive(Clone, Default)]
pub struct DiagSources {
    db: Option<Arc<Db>>,
    app_dirs: Option<gento_core::AppDirs>,
    thumbnails: Option<Arc<crate::thumbnails::ThumbnailService>>,
    embedding: Option<Arc<crate::embedding::EmbeddingService>>,
    vector_index: Option<Arc<crate::vectorindex::VectorIndexService>>,
}

impl DiagSources {
    pub fn of(state: &AppState) -> Self {
        DiagSources {
            db: state.db.as_ref().and_then(|d| d.db.clone()),
            app_dirs: state.app_dirs.clone(),
            thumbnails: state.thumbnails.clone(),
            embedding: state.embedding.clone(),
            vector_index: state.vector_index.clone(),
        }
    }
}

fn repo_db_err(e: impl std::fmt::Display) -> gento_core::repo::RepoError {
    gento_core::repo::RepoError::Db(e.to_string())
}

/// quick_checkを実行して要約 (FTS5検査の内部書き込みのためwriter接続)。
fn quick_check_summary(db: &Db) -> String {
    db.with_writer(|repos| {
        let mut stmt = repos
            .conn()
            .prepare("PRAGMA quick_check")
            .map_err(repo_db_err)?;
        let mut rows = stmt.query([]).map_err(repo_db_err)?;
        let mut messages: Vec<String> = Vec::new();
        while let Some(row) = rows.next().map_err(repo_db_err)? {
            messages.push(row.get(0).map_err(repo_db_err)?);
        }
        Ok(summarize_quick_check(messages))
    })
    .unwrap_or_else(|e| format!("quick_check unavailable: {e}"))
}

fn summarize_quick_check(messages: Vec<String>) -> String {
    if messages.is_empty() || messages.iter().all(|m| m == "ok") {
        return "ok".to_string();
    }
    messages
        .iter()
        .take(20)
        .cloned()
        .collect::<Vec<_>>()
        .join("; ")
}

fn collect_db_area(sources: &DiagSources) -> DbAreaDto {
    match &sources.db {
        Some(db) => {
            let settings = gento_db::load_app_settings(db);
            let schema_version = db
                .with_reader(|repos| {
                    repos
                        .conn()
                        .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                        .map_err(repo_db_err)
                })
                .unwrap_or(0);
            DbAreaDto {
                db_open: true,
                integrity_check: quick_check_summary(db),
                schema_version,
                db_bytes: file_len(db.path()),
                wal_bytes: file_len(&wal_path(db.path())),
                settings_corrupt_count: settings.corrupt_count(),
                db_path: db.path().display().to_string(),
            }
        }
        None => DbAreaDto {
            db_open: false,
            integrity_check: "db unavailable".to_string(),
            schema_version: 0,
            db_bytes: 0,
            wal_bytes: 0,
            settings_corrupt_count: 0,
            db_path: String::new(),
        },
    }
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn wal_path(db: &Path) -> PathBuf {
    let mut s = db.as_os_str().to_os_string();
    s.push("-wal");
    PathBuf::from(s)
}

fn fts_area_unavailable(last_rebuild_at: Option<String>) -> FtsAreaDto {
    FtsAreaDto {
        expected_docs: 0,
        actual_docs: 0,
        missing_docs: 0,
        orphan_docs: 0,
        duplicate_docs: 0,
        stale_contents: 0,
        consistent: false,
        last_rebuild_at,
    }
}

fn collect_fts_area(sources: &DiagSources) -> FtsAreaDto {
    let Some(db) = &sources.db else {
        return fts_area_unavailable(None);
    };
    let last_rebuild_at = gento_db::load_app_settings(db).fts_last_rebuild_at;
    let Some(report) = db
        .with_reader(|repos| gento_db::fts_integrity(repos.conn()))
        .ok()
    else {
        return fts_area_unavailable(last_rebuild_at);
    };
    FtsAreaDto {
        expected_docs: report.expected_docs,
        actual_docs: report.actual_docs,
        missing_docs: report.missing_docs,
        orphan_docs: report.orphan_docs,
        duplicate_docs: report.duplicate_docs,
        stale_contents: report.stale_contents,
        consistent: report.consistent,
        last_rebuild_at,
    }
}

fn collect_usearch_area(sources: &DiagSources) -> UsearchAreaDto {
    let Some(status) = sources.vector_index.as_ref().map(|s| s.status()) else {
        return UsearchAreaDto {
            model: String::new(),
            indexed_count: 0,
            sqlite_count: 0,
            stale: false,
            dimensions: None,
            ready: false,
            building: false,
            last_error: Some("埋め込みが無効です".to_string()),
        };
    };
    let dimensions = if status.model.is_empty() {
        None
    } else {
        crate::vectorindex::identity_of_model(&status.model).map(|i| i.dimensions as u32)
    };
    UsearchAreaDto {
        model: status.model,
        indexed_count: status.indexed_count,
        sqlite_count: status.sqlite_count,
        stale: status.indexed_count != status.sqlite_count,
        dimensions,
        ready: status.ready,
        building: status.building,
        last_error: status.last_error,
    }
}

fn collect_models_area(sources: &DiagSources) -> ModelsAreaDto {
    let Some(status) = sources.embedding.as_ref().map(|s| s.status()) else {
        return ModelsAreaDto {
            enabled: false,
            packages: Vec::new(),
            worker_phase: "not_started".to_string(),
            worker_restarts: 0,
            worker_last_error: None,
            loaded_model: None,
        };
    };
    ModelsAreaDto {
        enabled: status.enabled,
        packages: status
            .packages
            .iter()
            .map(|p| ModelPackageDto {
                model_id: p.model_id.clone(),
                state: p.state.clone(),
                embedding_dim: p.embedding_dim as u32,
                license: p.license.clone(),
            })
            .collect(),
        worker_phase: serde_json::to_value(status.worker.phase)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default(),
        worker_restarts: status.worker.restart_count,
        worker_last_error: status.worker.last_error.clone(),
        loaded_model: status.loaded_model.clone(),
    }
}

fn collect_jobs_area(sources: &DiagSources) -> JobsAreaDto {
    let Some(db) = &sources.db else {
        return JobsAreaDto {
            by_type: Vec::new(),
            oldest_pending_age_seconds: None,
        };
    };
    db.with_reader(|repos| {
        let conn = repos.conn();
        let by_type: Vec<JobCountDto> = {
            let mut stmt = conn
                .prepare(
                    "SELECT job_type, status, count(*) FROM jobs \
                     GROUP BY job_type, status ORDER BY job_type, status",
                )
                .map_err(repo_db_err)?;
            let rows = stmt
                .query_map([], |row| {
                    Ok(JobCountDto {
                        job_type: row.get(0)?,
                        status: row.get(1)?,
                        count: row.get::<_, i64>(2)?.max(0) as u64,
                    })
                })
                .map_err(repo_db_err)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(repo_db_err)?;
            rows
        };
        let oldest: Option<String> = conn
            .query_row(
                "SELECT min(created_at) FROM jobs WHERE status IN ('queued','retry_waiting')",
                [],
                |r| r.get(0),
            )
            .map_err(repo_db_err)?;
        let age = oldest.and_then(|created| {
            parse_rfc3339_epoch_secs(&created).map(|secs| {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                now.saturating_sub(secs as u64)
            })
        });
        Ok(JobsAreaDto {
            by_type,
            oldest_pending_age_seconds: age,
        })
    })
    .unwrap_or(JobsAreaDto {
        by_type: Vec::new(),
        oldest_pending_age_seconds: None,
    })
}

/// RFC3339 (ids.rsの整形形式 `YYYY-MM-DDTHH:MM:SS.mmmZ`) をepoch秒へ。
/// 不正形式はNone (診断は継続)。
fn parse_rfc3339_epoch_secs(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() != 24 || bytes[10] != b'T' || bytes[23] != b'Z' || bytes[19] != b'.' {
        return None;
    }
    let digit = |range: std::ops::Range<usize>| -> Option<i64> {
        let text = value.get(range)?;
        if !text.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        text.parse::<i64>().ok()
    };
    let (y, mo, d) = (digit(0..4)?, digit(5..7)?, digit(8..10)?);
    let (h, mi, s) = (digit(11..13)?, digit(14..16)?, digit(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    // Hinnant days_from_civil (ids.rs civil_from_daysの逆算)
    let y = if mo <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3_600 + mi * 60 + s)
}

/// 5領域スナップショットを収集する (read-only + quick_checkのみwriter)。
pub fn collect_diagnostics(sources: &DiagSources) -> DiagnosticsDto {
    DiagnosticsDto {
        db: collect_db_area(sources),
        fts: collect_fts_area(sources),
        usearch: collect_usearch_area(sources),
        models: collect_models_area(sources),
        jobs: collect_jobs_area(sources),
    }
}

// ===== bundle =====

/// 既知パス → プレースホルダの置換表 (長い順に適用)。
fn path_placeholders(sources: &DiagSources) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    fn push(pairs: &mut Vec<(String, String)>, path: &Path, label: &str) {
        let text = path.display().to_string();
        if !text.is_empty() && !text.ends_with('/') {
            pairs.push((text, format!("<{label}>")));
        }
    }
    // ライブラリルート (DB登録順に番号)
    if let Some(db) = &sources.db {
        let roots = db
            .with_reader(|repos| repos.list_roots())
            .unwrap_or_default();
        for (index, root) in roots.iter().enumerate() {
            let text = root.path.display().to_string();
            if !text.is_empty() {
                pairs.push((text, format!("<root:{}>", index + 1)));
            }
        }
        push(&mut pairs, db.path(), "db");
    }
    if let Some(dirs) = sources.app_dirs.as_ref() {
        push(&mut pairs, &dirs.data_dir, "data");
        push(&mut pairs, &dirs.cache_dir, "cache");
        push(&mut pairs, &dirs.log_dir, "log");
    }
    if let Some(thumbnails) = sources.thumbnails.as_ref() {
        push(&mut pairs, thumbnails.cache().root(), "thumbnails");
    }
    if let Some(embedding) = sources.embedding.as_ref() {
        push(&mut pairs, embedding.models_dir(), "models");
    }
    if let Some(home) = dirs::home_dir() {
        push(&mut pairs, &home, "home");
    }
    // 長い順に適用 (短いprefixが先に置換され、長い実パスが残るのを防ぐ)
    pairs.sort_by_key(|(path, _)| std::cmp::Reverse(path.len()));
    pairs.dedup_by(|a, b| a.0 == b.0);
    pairs
}

/// JSON値ツリー内の全ての文字列へscrubを適用する。
fn scrub_walk(value: &mut Value) {
    match value {
        Value::String(text) => {
            *text = gento_core::redact::scrub_text(text);
        }
        Value::Array(items) => {
            for item in items {
                scrub_walk(item);
            }
        }
        Value::Object(map) => {
            for (_, item) in map.iter_mut() {
                scrub_walk(item);
            }
        }
        _ => {}
    }
}

/// bundle本文を構築する (scrub適用前の状態で組み立て、書き出し直前に
/// scrub + パス置換を通す)。
fn bundle_value(sources: &DiagSources, ring: Option<&crate::logging::LogRing>) -> Value {
    let settings = sources
        .db
        .as_ref()
        .map(|db| gento_db::load_app_settings(db));
    let settings = settings.as_ref().map(|s| {
        (
            s.general.log_level.as_str(),
            s.cache.thumbs_max_bytes,
            s.backup.auto_full_db.as_str(),
            s.backup.retention_generations,
            s.settings_version,
            s.corrupt_keys.clone(),
        )
    });
    let (log_level, thumbs_max, auto_full_db, retention, settings_version, corrupt_keys) =
        match settings {
            Some(s) => s,
            None => (
                gento_db::LogLevel::Info.as_str(),
                gento_db::DEFAULT_THUMBS_MAX_BYTES,
                "off",
                3,
                gento_db::SETTINGS_SCHEMA_VERSION,
                Vec::new(),
            ),
        };
    let diagnostics = collect_diagnostics(sources);
    let log_tail: Vec<String> = ring
        .map(|r| r.tail(BUNDLE_LOG_TAIL_LINES))
        .unwrap_or_default();
    json!({
        "format_version": BUNDLE_FORMAT_VERSION,
        "app_version": env!("CARGO_PKG_VERSION"),
        "api_version": GENTO_API_VERSION,
        "schema_version": diagnostics.db.schema_version,
        "generated_at": gento_db::rfc3339_after_now(0),
        // 設定 (APIキー類は元々設定へ保存されないため構造的に不含。
        // AIプロファイル・鍵参照も対象外)
        "settings": {
            "general": { "log_level": log_level },
            "cache": { "thumbs_max_bytes": thumbs_max },
            "backup": {
                "auto_full_db": auto_full_db,
                "retention_generations": retention,
            },
            "settings_version": settings_version,
            "corrupt_keys": corrupt_keys,
        },
        "diagnostics": serde_json::to_value(&diagnostics).unwrap_or(Value::Null),
        // 直近ログ (redaction済みログ出力そのもの。さらにscrubを通す)
        "log_tail": log_tail,
    })
}

/// bundle書き出しの実行排他 (同一プロセス内の並行生成でfinal名/tmp名の
/// 競合を構造的に排除 — FG-WP16-3 N4。跨プロセス競合はtmpのuuid付与と
/// rename_noreplaceが最終防御)。
static BUNDLE_WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// bundleを書き出す (tmp書き→sync→`rename_noreplace` 公開。
/// 同一stampの衝突は連番回避。FG-WP16-3 N4)。
fn write_bundle_atomic(dest_dir: &Path, value: &Value) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dest_dir)
        .map_err(|e| format!("出力先ディレクトリを作成できません: {e}"))?;
    let _guard = BUNDLE_WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let stamp = gento_db::rfc3339_after_now(0).replace([':', '.'], "-");
    // tmp名はuuid付き (PIDのみでは同一プロセス内の並行生成で衝突する)
    let tmp_path = dest_dir.join(format!(
        ".gento-diagnostics-{stamp}-{}.tmp-{}",
        uuid::Uuid::now_v7().simple(),
        std::process::id()
    ));
    let body = serde_json::to_string_pretty(value)
        .map_err(|e| format!("診断bundleの直列化に失敗しました: {e}"))?;
    let write = std::fs::write(&tmp_path, body.as_bytes()).and_then(|()| {
        std::fs::File::open(&tmp_path)?.sync_all()?;
        Ok(())
    });
    if let Err(e) = write {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(format!("診断bundleの書き込みに失敗しました: {e}"));
    }
    // no-replaceで出版。同一stampの先任者がいれば連番で回避する
    // (exists()+renameのTOCTOU窓を排する)。
    let mut suffix: u32 = 0;
    loop {
        let name = if suffix == 0 {
            format!("gento-diagnostics-{stamp}.json")
        } else {
            format!("gento-diagnostics-{stamp}-{suffix}.json")
        };
        let final_path = dest_dir.join(&name);
        match gento_platform::rename_noreplace(&tmp_path, &final_path) {
            Ok(()) => {
                // 親ディレクトリのエントリ反映 (best-effort)
                if let Ok(handle) = std::fs::File::open(dest_dir) {
                    let _ = handle.sync_all();
                }
                return Ok(final_path);
            }
            Err(gento_platform::RenameNoReplaceError::DestinationExists) => {
                suffix += 1;
                continue;
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp_path);
                return Err(format!("診断bundleの公開に失敗しました: {e}"));
            }
        }
    }
}

/// bundle生成の本体 (commandとテストから呼ぶ)。
pub fn build_bundle(
    sources: &DiagSources,
    ring: Option<&crate::logging::LogRing>,
    dest_dir: &Path,
) -> Result<BundleResultDto, String> {
    let mut value = bundle_value(sources, ring);
    // 全文字列の秘密パターン除去 → 直列化 → パスプレースホルダ置換
    scrub_walk(&mut value);
    let mut text = serde_json::to_string_pretty(&value)
        .map_err(|e| format!("診断bundleの直列化に失敗しました: {e}"))?;
    for (path, placeholder) in path_placeholders(sources) {
        text = text.replace(&path, &placeholder);
    }
    // 既知dir以外の絶対パス (ログ行に混入した作業dir等) も最終的に伏せる
    text = gento_core::redact::scrub_paths(&text);
    // 置換後もvalid JSONであることを保証 (置換は文字列値の中のみを
    // 書き換えるため崩れない — 壊れていたら出荷しない)
    let final_value: Value =
        serde_json::from_str(&text).map_err(|e| format!("診断bundleの検証に失敗しました: {e}"))?;
    let path = write_bundle_atomic(dest_dir, &final_value)?;
    Ok(BundleResultDto {
        path: path.display().to_string(),
    })
}

// ===== Commands =====

/// 5領域の診断スナップショット (read-only)。
#[tauri::command]
pub fn diagnostics_snapshot(state: State<'_, AppState>) -> DiagnosticsDto {
    collect_diagnostics(&DiagSources::of(&state))
}

/// 診断bundleをユーザー指定dirへ生成 (秘密・実パス除外済みJSON 1ファイル)。
#[tauri::command]
pub async fn diagnostics_bundle_create(
    state: State<'_, AppState>,
    dest_dir: String,
) -> Result<BundleResultDto, String> {
    let sources = DiagSources::of(&state);
    let ring = crate::logging::ring();
    let dest = PathBuf::from(dest_dir);
    tauri::async_runtime::spawn_blocking(move || build_bundle(&sources, ring.as_deref(), &dest))
        .await
        .map_err(|e| format!("診断bundleの生成に失敗しました: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use gento_core::assetmeta::AssetMetadataRepo;
    use gento_core::repo::{AssetRepo, FileRepo, FileUpsert, NewAsset, ScanState};
    use gento_core::AppDirs;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "gento-tauri-diag-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn test_png(path: &std::path::Path) {
        use image::{Rgb, RgbImage};
        let img = RgbImage::from_fn(32, 24, |x, _| Rgb([(x % 256) as u8, 0x80, 0x40]));
        img.write_with_encoder(image::codecs::png::PngEncoder::new(
            std::fs::File::create(path).unwrap(),
        ))
        .unwrap();
    }

    /// DB + ルート + asset + FTS文書構築済みのDiagSources。
    fn sources_with_library(tag: &str) -> (TempDir, DiagSources, i64) {
        let tmp = TempDir::new(tag);
        let root_dir = tmp.path().join("photos");
        std::fs::create_dir_all(&root_dir).unwrap();
        test_png(&root_dir.join("a.png"));
        let db = Arc::new(Db::open(tmp.path().join("db.sqlite3")).unwrap());
        let (_root_id, asset_id) = db
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
                    width: Some(32),
                    height: Some(24),
                    has_alpha: Some(false),
                    is_animated: Some(false),
                    frame_count: None,
                })?;
                repos.link_file(file.id, asset.id)?;
                Ok((root.id, asset.id))
            })
            .unwrap();
        // FTS文書を構築 (整合状態の対照)
        db.with_writer(|repos| gento_db::fts_rebuild(repos.conn(), &mut |_, _| {}))
            .unwrap();
        let sources = DiagSources {
            db: Some(db),
            ..DiagSources::default()
        };
        (tmp, sources, asset_id)
    }

    struct NoopEvents;
    impl crate::thumbnails::ThumbEvents for NoopEvents {
        fn ready(&self, _items: &[crate::thumbnails::ThumbReadyItem]) {}
        fn failed(&self, _items: &[crate::thumbnails::ThumbFailedItem]) {}
    }

    #[test]
    fn parse_rfc3339_matches_epoch_known_values() {
        // 2026-09-03T00:00:00.000Z = 1788393600 (UTC)
        assert_eq!(
            parse_rfc3339_epoch_secs("2026-09-03T00:00:00.000Z"),
            Some(1_788_393_600)
        );
        assert_eq!(
            parse_rfc3339_epoch_secs("1970-01-01T00:00:00.000Z"),
            Some(0)
        );
        assert_eq!(
            parse_rfc3339_epoch_secs("2026-09-03T00:00:00.001Z"),
            Some(1_788_393_600)
        );
        assert_eq!(parse_rfc3339_epoch_secs("bogus"), None);
        assert_eq!(parse_rfc3339_epoch_secs("2026-13-40T00:00:00.000Z"), None);
    }

    #[test]
    fn snapshot_db_fts_jobs_areas_report_healthy_state() {
        let (_tmp, sources, _asset) = sources_with_library("healthy");
        let dto = collect_diagnostics(&sources);
        assert!(dto.db.db_open);
        assert_eq!(dto.db.integrity_check, "ok");
        assert_eq!(dto.db.schema_version, gento_db::LATEST_VERSION as i64);
        assert!(dto.db.db_bytes > 0);
        assert_eq!(dto.db.settings_corrupt_count, 0);
        assert!(dto.fts.consistent);
        assert_eq!(dto.fts.expected_docs, 1);
        assert_eq!(dto.fts.actual_docs, 1);
        assert!(dto.fts.last_rebuild_at.is_none(), "未再構築時はnull");
        // usearch無し (embedding service無し) でも領域は出る
        assert_eq!(dto.usearch.indexed_count, 0);
        assert_eq!(dto.usearch.sqlite_count, 0);
        assert!(!dto.usearch.stale);
        assert_eq!(dto.models.worker_phase, "not_started");
        // ジョブ無し
        assert!(dto.jobs.by_type.is_empty());
        assert_eq!(dto.jobs.oldest_pending_age_seconds, None);
    }

    #[test]
    fn snapshot_detects_broken_fts_as_inconsistent() {
        let (_tmp, sources, _asset) = sources_with_library("fts-broken");
        // 文書を欠落させる (破損注入)
        sources
            .db
            .as_ref()
            .unwrap()
            .with_writer(|repos| {
                repos
                    .conn()
                    .execute("DELETE FROM asset_docs", [])
                    .map_err(repo_db_err)
            })
            .unwrap();
        let dto = collect_diagnostics(&sources);
        assert!(!dto.fts.consistent, "文書欠落は不整合として検出");
        assert!(dto.fts.missing_docs >= 1, "{}", dto.fts.missing_docs);
    }

    #[test]
    fn snapshot_reports_oldest_pending_job_age() {
        let (_tmp, sources, asset) = sources_with_library("jobs");
        sources
            .db
            .as_ref()
            .unwrap()
            .with_writer(|repos| {
                repos.enqueue_job("ai_caption_tags", "asset", asset, 5, r#"{"profile_id":1}"#)
            })
            .unwrap();
        let dto = collect_diagnostics(&sources);
        assert_eq!(dto.jobs.by_type.len(), 1);
        assert_eq!(dto.jobs.by_type[0].job_type, "ai_caption_tags");
        assert_eq!(dto.jobs.by_type[0].status, "queued");
        assert_eq!(dto.jobs.by_type[0].count, 1);
        let age = dto.jobs.oldest_pending_age_seconds.expect("待機中あり");
        assert!(age <= 60, "投入直後: {age}秒");
    }

    /// WP-15受け入れ「診断bundle生成時にパス、APIキー、画像内容を除外」:
    /// 鍵登録済みプロファイル・GPSメタデータ・実rootパス・危険なログ行を
    /// 意図的に混入した状態でbundleを生成し、出力JSONにそれらの部分文字列が
    /// 一度も出現しないことを検証する。
    #[test]
    fn bundle_excludes_secrets_gps_and_real_paths() {
        use gento_ai::{ApiKey, SecretStore};
        use gento_core::assetmeta::{AssetMetadataParsed, MetadataColumns};

        const KEY: &str = "sk-wp15-bundle-leak-probe-987654321";
        const GPS_LAT: f64 = 35.681236;
        const GPS_LON: f64 = 139.767125;

        let (tmp, mut sources, asset) = sources_with_library("bundle");
        let db = sources.db.clone().unwrap();

        // (1) APIキーを保存したプロファイル (鍵の実体が環境に存在する状態)
        let profile_id = db
            .with_writer(|repos| {
                repos.insert_ai_profile(&gento_db::NewAiProfile {
                    display_name: "leak-probe".to_string(),
                    base_url: "https://api.example.com/v1".to_string(),
                    model: "gpt-x".to_string(),
                    vision_capable: true,
                    enabled: true,
                    timeout_ms: 30_000,
                    concurrency: 2,
                    extra_json: None,
                })
            })
            .unwrap()
            .id;
        let stored = SecretStore::file_only(tmp.path())
            .save(
                "00000000-0000-7000-8000-0000000000ee",
                &ApiKey::new(KEY.to_string()),
            )
            .unwrap();
        db.with_writer(|repos| {
            repos.set_ai_profile_api_key_ref(profile_id, Some(&stored.as_ref_string()))
        })
        .unwrap();

        // (2) GPS座標入りメタデータ
        db.with_writer(|repos| {
            repos.save_asset_metadata(
                asset,
                &[AssetMetadataParsed {
                    parser_name: "exif".to_string(),
                    parser_version: "1".to_string(),
                    columns: MetadataColumns {
                        gps_latitude: Some(GPS_LAT),
                        gps_longitude: Some(GPS_LON),
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

        // (3) 実パス要素を含むapp_dirs + サムネイルキャッシュ
        let app_dirs = AppDirs {
            data_dir: tmp.path().join("data"),
            cache_dir: tmp.path().join("cache"),
            log_dir: tmp.path().join("cache").join("log"),
        };
        std::fs::create_dir_all(&app_dirs.data_dir).unwrap();
        sources.app_dirs = Some(app_dirs.clone());
        sources.thumbnails = Some(
            crate::thumbnails::ThumbnailService::new_with_limit(
                tmp.path().join("cache").join("thumbnails"),
                1 << 20,
                Box::new(NoopEvents),
            )
            .unwrap(),
        );

        // (4) 危険なログ行を混入したring (迂回経路のscrub・パス置換検証。
        //     実パス (root/data/thumbnails) と鍵とGPS座標を混ぜる)
        let ring = crate::logging::LogRing::for_test();
        let root_text = tmp.path().join("photos").display().to_string();
        let data_text = app_dirs.data_dir.display().to_string();
        let thumbs_text = app_dirs.cache_dir.join("thumbnails").display().to_string();
        ring.push_line_for_test(&format!("probe Authorization: Bearer {KEY} at {root_text}"));
        ring.push_line_for_test(&format!("probe coords lat={GPS_LAT} lon={GPS_LON}"));
        ring.push_line_for_test(&format!("probe dirs {data_text} {thumbs_text}"));

        // bundle生成
        let out_dir = tmp.path().join("exports");
        let result = build_bundle(&sources, Some(&ring), &out_dir).unwrap();
        let body = std::fs::read_to_string(&result.path).unwrap();

        // JSONとしてvalid
        let value: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["format_version"], BUNDLE_FORMAT_VERSION);
        assert_eq!(value["api_version"], GENTO_API_VERSION);

        // 鍵素材: 一度も出現しない
        assert!(!body.contains(KEY), "APIキー漏出: {body}");
        assert!(!body.contains("bundle-leak-probe-987654321"), "{body}");
        assert!(!body.contains("Bearer sk-"), "{body}");
        // GPS座標: 一度も出現しない
        assert!(!body.contains("35.681236"), "GPS緯度: {body}");
        assert!(!body.contains("139.767125"), "GPS経度: {body}");
        // 実パス: 一度も出現しない (root/data/cache/thumbnails/tmp)
        assert!(!body.contains(root_text.as_str()), "root実パス: {body}");
        let tmp_text = tmp.path().display().to_string();
        assert!(!body.contains(tmp_text.as_str()), "tmp実パス: {body}");
        assert!(!body.contains(data_text.as_str()), "data実パス: {body}");
        assert!(
            !body.contains(thumbs_text.as_str()),
            "thumbnails実パス: {body}"
        );
        // プレースホルダは機能している
        assert!(body.contains("<root:1>"), "root置換: {body}");
        assert!(body.contains("<data>"), "data置換: {body}");
        assert!(body.contains("<thumbnails>"), "{body}");
        // ログ末尾は (scrub済みで) 含まれる
        assert!(value["log_tail"].as_array().unwrap().len() >= 3);
        // 画像内容 (thumbnailバイナリ等) は構造的に不含 — テキストJSONで
        // ありbase64データURLを含まないこと
        assert!(!body.contains("data:image"), "{body}");
    }

    #[test]
    fn bundle_uses_tmp_sync_rename_without_leftovers() {
        let (tmp, sources, _asset) = sources_with_library("atomic");
        let out_dir = tmp.path().join("out");
        let result = build_bundle(&sources, None, &out_dir).unwrap();
        assert!(result.path.ends_with(".json"), "{}", result.path);
        assert!(Path::new(&result.path).is_file());
        // tmp残骸なし
        let leftovers: Vec<String> = std::fs::read_dir(&out_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        // 2回目も衝突しない
        let second = build_bundle(&sources, None, &out_dir).unwrap();
        assert_ne!(result.path, second.path);
    }

    /// FG-WP16-3 N4: 並行bundle生成は実行lock + no-replace rename + 連番で
    /// 競合しない — 各スレッドの成果物が全て異なる名前で残り、tmp残骸・
    /// 上書き失効が起きない。同一stamp (同一ミリ秒) での衝突を高確率で
    /// 起こすためバリア同期で一斉スタートする。
    #[test]
    fn concurrent_bundle_creations_get_distinct_names_without_clobber() {
        let (tmp, sources, _asset) = sources_with_library("concurrent");
        let out_dir = tmp.path().join("out");
        let sources = Arc::new(sources);
        const THREADS: usize = 8;
        let barrier = Arc::new(std::sync::Barrier::new(THREADS));
        let mut handles = Vec::new();
        for _ in 0..THREADS {
            let out_dir = out_dir.clone();
            let sources = Arc::clone(&sources);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                build_bundle(&sources, None, &out_dir)
            }));
        }
        let mut paths = Vec::new();
        for handle in handles {
            let result = handle.join().unwrap().unwrap_or_else(|e| {
                panic!("並行生成が失敗した (命名競合): {e}");
            });
            paths.push(result.path);
        }
        // 全て成功し、名前は相互に異なる
        let unique: std::collections::HashSet<&String> = paths.iter().collect();
        assert_eq!(unique.len(), paths.len(), "名前重複: {paths:?}");
        // 全ファイルが実在し、validなbundleである
        let mut json_count = 0;
        let mut tmp_count = 0;
        for entry in std::fs::read_dir(&out_dir).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            if name.ends_with(".json") {
                let body = std::fs::read_to_string(out_dir.join(&name)).unwrap();
                let value: Value = serde_json::from_str(&body).unwrap();
                assert_eq!(value["format_version"], BUNDLE_FORMAT_VERSION, "{name}");
                json_count += 1;
            } else if name.contains(".tmp-") {
                tmp_count += 1;
            }
        }
        assert_eq!(json_count, THREADS, "全スレッド分が独立に出版されている");
        assert_eq!(tmp_count, 0, "tmp残骸なし");
    }

    #[test]
    fn db_area_reports_settings_corruption_count() {
        let (_tmp, sources, _asset) = sources_with_library("corrupt-count");
        sources
            .db
            .as_ref()
            .unwrap()
            .with_writer(|repos| repos.setting_set("general.log_level", "not-a-level"))
            .unwrap();
        let dto = collect_diagnostics(&sources);
        assert_eq!(dto.db.settings_corrupt_count, 1);
    }

    /// usearch領域の自己整合: stale/readyは件数差から正しく導かれる
    /// (service有無によらず内部矛盾しない)。
    #[test]
    fn usearch_area_flags_derive_from_counts() {
        let (_tmp, sources, _asset) = sources_with_library("usearch-shape");
        let dto = collect_diagnostics(&sources);
        assert_eq!(
            dto.usearch.stale,
            dto.usearch.indexed_count != dto.usearch.sqlite_count
        );
        if dto.usearch.ready {
            assert_eq!(dto.usearch.indexed_count, dto.usearch.sqlite_count);
        }
    }
}
