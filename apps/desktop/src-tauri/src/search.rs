//! 検索command群 (MVP WP-8)。FTS5検索の実行・一致根拠・索引保守。
//!
//! thin adapter: AST parser (gento-search) と検索実行 (gento-db search) を
//! 組み合わせ、結果をカタログの検索ソースとして表示する。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use gento_search::{parse as parse_query, SearchQuery, SearchTerm};
use serde::Serialize;
use serde_json::Value as JsonValue;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::catalog::{CatalogChangeReason, CatalogChangedPayload, CatalogSummary, SearchDisplay};
use crate::AppState;

/// 検索結果表示に載せる上限 (5万件規模 + 余裕。それ超過は打ち切り)。
const SEARCH_VIEW_LIMIT: u64 = 100_000;
/// asset id一覧の取得チャンク。
const SEARCH_PAGE: u32 = 5_000;

fn db_of(state: &AppState) -> Result<Arc<gento_db::Db>, String> {
    state
        .db
        .as_ref()
        .and_then(|d| d.db.clone())
        .ok_or_else(|| "データベースが利用できないため検索できません".to_string())
}

/// GUIフィルタ (AST termのJSON列) を検証してAST termへ変換する。
/// 各要素はgento-search ASTのterm直列化 ({"type":"field",...}) と同一形状。
fn filter_terms(filters: Option<Vec<JsonValue>>) -> Result<Vec<SearchTerm>, String> {
    let Some(filters) = filters else {
        return Ok(Vec::new());
    };
    let mut terms = Vec::with_capacity(filters.len());
    for filter in filters {
        let term: SearchTerm = serde_json::from_value(filter)
            .map_err(|e| format!("フィルタ条件の形式が不正です: {e}"))?;
        terms.push(term);
    }
    Ok(terms)
}

/// `search_run` の応答。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchRunDto {
    pub summary: CatalogSummary,
    /// 検索一致総数 (表示上限適用前)。
    pub total: u64,
    /// 実際に表示へ載せた件数 (上限適用後)。
    pub shown: u64,
    /// 実行時間 (ms)。
    pub elapsed_ms: u64,
    /// 未対応キー等の警告 (検索は実行される)。
    pub warnings: Vec<String>,
    /// 保存済み検索 (スマートコレクション) 作成用のAST JSON (version 2)。
    pub query_json: String,
}

/// `search_evidence` の応答項目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchEvidenceDto {
    pub asset_id: String,
    pub matches: Vec<SearchFieldMatch>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchFieldMatch {
    /// "filename" | "path" | "tags" | "captions" | "note"
    pub field: String,
    /// 一致箇所の前後文 ([一致語] 形式)。
    pub snippet: String,
}

/// `fts_integrity` の応答。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FtsIntegrityDto {
    pub expected_docs: u64,
    pub actual_docs: u64,
    pub missing_docs: u64,
    pub orphan_docs: u64,
    pub duplicate_docs: u64,
    pub stale_contents: u64,
    pub consistent: bool,
}

impl From<gento_db::FtsIntegrityReport> for FtsIntegrityDto {
    fn from(report: gento_db::FtsIntegrityReport) -> Self {
        Self {
            expected_docs: report.expected_docs,
            actual_docs: report.actual_docs,
            missing_docs: report.missing_docs,
            orphan_docs: report.orphan_docs,
            duplicate_docs: report.duplicate_docs,
            stale_contents: report.stale_contents,
            consistent: report.consistent,
        }
    }
}

/// `fts_rebuild` / `fts_repair` の応答。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FtsMaintenanceDto {
    /// 再構築 (または修復) した文書数。
    pub rebuilt: u64,
    /// 削除した孤立・重複文書数 (repairのみ)。
    pub removed: u64,
    pub elapsed_ms: u64,
}

/// `fts:rebuild_progress` イベントのペイロード。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FtsProgressPayload {
    pub done: u64,
    pub total: u64,
}

/// 検索を実行して結果をカタログの検索ソースとして表示する (WP-8)。
///
/// - `query`: 検索構文文字列 (api-contract §42)
/// - `filters`: GUIフィルタ (AST term JSON列。クエリとAND合成)
/// - `scope_root_id` / `scope_collection_id`: 検索範囲
///   (両方None = ライブラリ全体)
#[tauri::command]
pub fn search_run(
    app: AppHandle,
    state: State<'_, AppState>,
    query: String,
    filters: Option<Vec<JsonValue>>,
    scope_root_id: Option<String>,
    scope_collection_id: Option<String>,
) -> Result<SearchRunDto, String> {
    let db = db_of(&state)?;
    let started = Instant::now();

    let scope_root = scope_root_id
        .as_deref()
        .map(|value| value.parse::<i64>())
        .transpose()
        .map_err(|_| "検索範囲のルートIDが不正です".to_string())?;
    let scope_collection = scope_collection_id
        .as_deref()
        .map(|value| value.parse::<i64>())
        .transpose()
        .map_err(|_| "検索範囲のコレクションIDが不正です".to_string())?;

    let outcome = parse_query(&query).map_err(|e| e.to_string())?;
    let mut ast = outcome.query;
    ast.terms.extend(filter_terms(filters)?);
    let query_json = ast.to_stored_json();

    let scope = gento_db::SearchScope {
        root_id: scope_root,
        collection_id: scope_collection,
        require_present_files: true,
    };
    let (asset_ids, total) = collect_search_ids(&db, &ast, &scope)?;
    let records = crate::collections::build_collection_records(&db, &asset_ids)
        .map_err(|e| format!("検索結果の表示構築に失敗しました: {e}"))?;
    let shown = records.len() as u64;

    let (summary, generation) = {
        let mut inner = crate::catalog::lock(&state.catalog);
        let generation = inner.set_search_display(SearchDisplay {
            query_json: query_json.clone(),
            scope_root_id: scope.root_id,
            scope_collection_id: scope.collection_id,
            records,
        });
        (crate::catalog::summary_of_pub(&inner), generation)
    };
    let _ = app.emit(
        "catalog:changed",
        CatalogChangedPayload {
            generation,
            reason: CatalogChangeReason::SearchSwitch,
        },
    );
    Ok(SearchRunDto {
        summary,
        total,
        shown,
        elapsed_ms: started.elapsed().as_millis() as u64,
        warnings: outcome.warnings,
        query_json,
    })
}

/// 検索一致全件のasset idをページングしながら集める (上限SEARCH_VIEW_LIMIT)。
fn collect_search_ids(
    db: &gento_db::Db,
    ast: &SearchQuery,
    scope: &gento_db::SearchScope,
) -> Result<(Vec<i64>, u64), String> {
    let mut ids: Vec<i64> = Vec::new();
    let mut offset: u32 = 0;
    let total = loop {
        let (page, total) = db
            .with_reader(|repos| {
                gento_db::search_asset_ids(repos.conn(), ast, scope, offset, SEARCH_PAGE)
            })
            .map_err(|e| format!("検索の実行に失敗しました: {e}"))?;
        let page_empty = page.is_empty();
        ids.extend(page);
        if ids.len() as u64 >= total.min(SEARCH_VIEW_LIMIT) || page_empty {
            break total;
        }
        offset += SEARCH_PAGE;
    };
    ids.truncate(SEARCH_VIEW_LIMIT as usize);
    Ok((ids, total))
}

/// 表示中検索の一致根拠を返す (InfoPanel・セルツールチップ用)。
/// 検索表示が存在しない場合は空配列 (エラーにしない — 表示切替直後等)。
#[tauri::command]
pub fn search_evidence(
    state: State<'_, AppState>,
    asset_ids: Vec<String>,
) -> Result<Vec<SearchEvidenceDto>, String> {
    if asset_ids.is_empty() {
        return Ok(Vec::new());
    }
    let query_json = {
        let inner = crate::catalog::lock(&state.catalog);
        inner
            .search_display()
            .map(|display| display.query_json.clone())
    };
    let Some(query_json) = query_json else {
        return Ok(Vec::new());
    };
    let query = match gento_search::stored::parse_stored(&query_json) {
        Ok(gento_search::stored::StoredSearch::Ast(query)) => query,
        Ok(gento_search::stored::StoredSearch::Legacy(legacy)) => {
            gento_search::stored::legacy_to_query(&legacy)
        }
        Err(e) => return Err(format!("保存済み検索条件が読み込めません: {e}")),
    };
    // 表示中の安定AssetId (10進文字列) → asset行ID
    let mut wanted: Vec<i64> = Vec::with_capacity(asset_ids.len());
    for id in &asset_ids {
        match id.parse::<i64>() {
            Ok(row_id) => wanted.push(row_id),
            Err(_) => return Err(format!("無効な画像IDです: {id}")),
        }
    }
    let db = db_of(&state)?;
    let evidence = db
        .with_reader(|repos| gento_db::search_evidence(repos.conn(), &wanted, &query))
        .map_err(|e| format!("一致根拠の取得に失敗しました: {e}"))?;
    Ok(evidence
        .into_iter()
        .map(|(asset_id, fields)| SearchEvidenceDto {
            asset_id: asset_id.to_string(),
            matches: fields
                .into_iter()
                .map(|evidence| SearchFieldMatch {
                    field: evidence.field.as_str().to_string(),
                    snippet: evidence.snippet,
                })
                .collect(),
        })
        .collect())
}

/// FTS索引とDB正本の整合性を検査する。
#[tauri::command]
pub fn fts_integrity(state: State<'_, AppState>) -> Result<FtsIntegrityDto, String> {
    let db = db_of(&state)?;
    let report = db
        .with_reader(|repos| gento_db::fts_integrity(repos.conn()))
        .map_err(|e| format!("索引診断に失敗しました: {e}"))?;
    Ok(report.into())
}

/// FTS索引を全再構築する (進捗は `fts:rebuild_progress` イベント)。
#[tauri::command]
pub async fn fts_rebuild(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<FtsMaintenanceDto, String> {
    let db = db_of(&state)?;
    let started = Instant::now();
    let app_for_job = app.clone();
    let rebuilt = tauri::async_runtime::spawn_blocking(move || {
        db.with_writer(|repos| {
            gento_db::fts_rebuild(repos.conn(), &mut |done, total| {
                let _ =
                    app_for_job.emit("fts:rebuild_progress", FtsProgressPayload { done, total });
            })
        })
    })
    .await
    .map_err(|e| format!("索引再構築が失敗しました: {e}"))?
    .map_err(|e| format!("索引再構築が失敗しました: {e}"))?;
    Ok(FtsMaintenanceDto {
        rebuilt,
        removed: 0,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

/// FTS索引の不整合のみを修復する (欠落・内容不一致の再構築、孤立・重複の削除)。
#[tauri::command]
pub fn fts_repair(state: State<'_, AppState>) -> Result<FtsMaintenanceDto, String> {
    let db = db_of(&state)?;
    let started = Instant::now();
    let (rebuilt, removed) = db
        .with_writer(|repos| gento_db::fts_repair(repos.conn()))
        .map_err(|e| format!("索引修復に失敗しました: {e}"))?;
    Ok(FtsMaintenanceDto {
        rebuilt,
        removed,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

/// 現在の検索表示を新しいDB状態で再実行する (表示中の明示refresh)。
#[tauri::command]
pub fn search_refresh(app: AppHandle, state: State<'_, AppState>) -> Result<SearchRunDto, String> {
    let display = {
        let inner = crate::catalog::lock(&state.catalog);
        inner.search_display().cloned()
    };
    let Some(display) = display else {
        return Err("再実行できる検索がありません".to_string());
    };
    let query = match gento_search::stored::parse_stored(&display.query_json) {
        Ok(gento_search::stored::StoredSearch::Ast(query)) => query,
        Ok(gento_search::stored::StoredSearch::Legacy(legacy)) => {
            gento_search::stored::legacy_to_query(&legacy)
        }
        Err(e) => return Err(format!("保存済み検索条件が読み込めません: {e}")),
    };
    let db = db_of(&state)?;
    let started = Instant::now();
    let scope = gento_db::SearchScope {
        root_id: display.scope_root_id,
        collection_id: display.scope_collection_id,
        require_present_files: true,
    };
    let (asset_ids, total) = collect_search_ids(&db, &query, &scope)?;
    let records = crate::collections::build_collection_records(&db, &asset_ids)
        .map_err(|e| format!("検索結果の表示構築に失敗しました: {e}"))?;
    let shown = records.len() as u64;
    let (summary, generation) = {
        let mut inner = crate::catalog::lock(&state.catalog);
        let generation = inner.set_search_display(SearchDisplay {
            query_json: display.query_json.clone(),
            scope_root_id: display.scope_root_id,
            scope_collection_id: display.scope_collection_id,
            records,
        });
        (crate::catalog::summary_of_pub(&inner), generation)
    };
    let _ = app.emit(
        "catalog:changed",
        CatalogChangedPayload {
            generation,
            reason: CatalogChangeReason::SearchSwitch,
        },
    );
    Ok(SearchRunDto {
        summary,
        total,
        shown,
        elapsed_ms: started.elapsed().as_millis() as u64,
        warnings: Vec::new(),
        query_json: display.query_json,
    })
}

/// 走査後のFTS保守が走行中か (多重起動防止)。
static FTS_MAINTENANCE_RUNNING: AtomicBool = AtomicBool::new(false);

/// ライブラリ走査後の索引保守 (WP-8: 初期索引は走査処理に組み込む)。
///
/// 通常の走査では文書はlink_file等のフックで同一tx構築済みのため
/// 何もしない (件数照合のみ・軽量)。WP-8より前のDB (asset_docs空) や
/// 不整合が検出された場合のみ背景スレッドで修復する。本格的な進捗UIは
/// WP-10のジョブ基盤で本化する (ここではtracingログのみ)。
pub fn ensure_fts_after_scan(app: &AppHandle) {
    let state = app.state::<AppState>();
    let Some(db) = state.db.as_ref().and_then(|d| d.db.clone()) else {
        return;
    };
    if FTS_MAINTENANCE_RUNNING.load(Ordering::Relaxed) {
        return;
    }
    std::thread::spawn(move || {
        if FTS_MAINTENANCE_RUNNING.swap(true, Ordering::Relaxed) {
            return;
        }
        let result = db.with_reader(|repos| gento_db::fts_counts(repos.conn()));
        let needs_repair = matches!(result, Ok((expected, actual)) if expected != actual);
        if !needs_repair {
            FTS_MAINTENANCE_RUNNING.store(false, Ordering::Relaxed);
            return;
        }
        let outcome = db.with_writer(|repos| gento_db::fts_repair(repos.conn()));
        FTS_MAINTENANCE_RUNNING.store(false, Ordering::Relaxed);
        match outcome {
            Ok((rebuilt, removed)) => {
                tracing::info!(rebuilt, removed, "fts index repaired after scan");
            }
            Err(error) => {
                tracing::warn!(%error, "fts index maintenance after scan failed");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_terms_accept_ast_field_and_text_forms() {
        let filters = vec![
            serde_json::json!({"type": "field", "key": "rating", "op": "ge", "value": {"number": 3}}),
            serde_json::json!({"type": "text", "value": "夜景"}),
        ];
        let terms = filter_terms(Some(filters)).unwrap();
        assert_eq!(terms.len(), 2);
        assert!(matches!(&terms[0], SearchTerm::Field(_)));
        assert_eq!(
            &terms[1],
            &SearchTerm::Text {
                value: "夜景".to_string()
            }
        );
    }

    #[test]
    fn filter_terms_reject_garbage() {
        let error = filter_terms(Some(vec![serde_json::json!({"type": "nonsense"})])).unwrap_err();
        assert!(error.contains("フィルタ条件"), "{error}");
        assert!(filter_terms(None).unwrap().is_empty());
    }

    #[test]
    fn dto_serialization_shapes() {
        let dto = FtsIntegrityDto {
            expected_docs: 10,
            actual_docs: 9,
            missing_docs: 1,
            orphan_docs: 0,
            duplicate_docs: 0,
            stale_contents: 0,
            consistent: false,
        };
        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "expectedDocs": 10,
                "actualDocs": 9,
                "missingDocs": 1,
                "orphanDocs": 0,
                "duplicateDocs": 0,
                "staleContents": 0,
                "consistent": false,
            })
        );
        let evidence = SearchEvidenceDto {
            asset_id: "7".to_string(),
            matches: vec![SearchFieldMatch {
                field: "tags".to_string(),
                snippet: "…[night]…".to_string(),
            }],
        };
        let json = serde_json::to_value(&evidence).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "assetId": "7",
                "matches": [{"field": "tags", "snippet": "…[night]…"}],
            })
        );
    }
}
