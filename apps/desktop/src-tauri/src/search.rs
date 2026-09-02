//! 検索command群 (MVP WP-8 + WP-12ハイブリッド拡張)。FTS5検索の実行・
//! 一致根拠・索引保守。
//!
//! thin adapter: AST parser (gento-search) と検索実行 (gento-db search) を
//! 組み合わせ、結果をカタログの検索ソースとして表示する。
//! WP-12: `semantic_weight` (0..=1) によりキーワード×意味の重み付きRRF
//! ハイブリッド検索 (api-contract §46) と類似画像検索
//! (`similar_search_run`) を提供する。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gento_search::{parse as parse_query, weighted_rrf, SearchQuery, SearchTerm, TextPlan};
use serde::Serialize;
use serde_json::Value as JsonValue;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::catalog::{
    CatalogChangeReason, CatalogChangedPayload, CatalogSummary, SearchDisplay, SearchDisplayKind,
};
use crate::AppState;

/// 検索結果表示に載せる上限 (5万件規模 + 余裕。それ超過は打ち切り)。
const SEARCH_VIEW_LIMIT: u64 = 100_000;
/// asset id一覧の取得チャンク。
const SEARCH_PAGE: u32 = 5_000;
/// ハイブリッド検索で各側から取得する上位件数 (api-contract §46)。
const HYBRID_TOP_K: usize = 200;
/// 意味側クエリ埋め込み (モデル初回ロード込み) の待ち上限。
const EMBED_QUERY_WAIT: Duration = Duration::from_secs(45);
/// 類似画像検索の既定表示件数。
const SIMILAR_DEFAULT_K: usize = 50;

fn db_of(state: &AppState) -> Result<Arc<gento_db::Db>, String> {
    state
        .db
        .as_ref()
        .and_then(|d| d.db.clone())
        .ok_or_else(|| "データベースが利用できないため検索できません".to_string())
}

fn vector_index_of(
    state: &AppState,
) -> Result<Arc<crate::vectorindex::VectorIndexService>, String> {
    state
        .vector_index
        .clone()
        .ok_or_else(|| "ベクトル索引が利用できないため意味検索できません".to_string())
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

/// `search_run` / `similar_search_run` の応答。
#[derive(Debug, Clone, PartialEq, Serialize)]
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
    /// 表示種別: "query" (構文検索) | "similar" (類似画像。WP-12)。
    pub kind: String,
    /// 意味側スコア (assetId → cosine類似度。ハイブリッド/類似表示の
    /// 上位分のみ。InfoPanel「意味 xx%」表示用。WP-12)。
    pub semantic_scores: HashMap<String, f32>,
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

/// 検索を実行して結果をカタログの検索ソースとして表示する (WP-8 + WP-12)。
///
/// - `query`: 検索構文文字列 (api-contract §42)
/// - `filters`: GUIフィルタ (AST term JSON列。クエリとAND合成)
/// - `scope_root_id` / `scope_collection_id`: 検索範囲
///   (両方None = ライブラリ全体)
/// - `semantic_weight`: 意味重み w ∈ [0,1] (WP-12)。0/None = 純キーワード
///   (旧経路・完全互換)。0 < w で意味側 (USearch) と重み付きRRF融合。
///   埋め込み無効・索引未構築・モデルロード失敗時は警告付きで
///   キーワード検索へフォールバック (§8: キーワード検索は常時機能)。
///
/// async command: 一致全件の収集 (5万件級の広域クエリで数百ms) は
/// `spawn_blocking` でUIスレッド外で実行する (QA-WP8。WP-7
/// `file_ops_execute`・`fts_rebuild`と同じ方式)。
#[tauri::command]
pub async fn search_run(
    app: AppHandle,
    state: State<'_, AppState>,
    query: String,
    filters: Option<Vec<JsonValue>>,
    scope_root_id: Option<String>,
    scope_collection_id: Option<String>,
    semantic_weight: Option<f64>,
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
    let weight = gento_search::clamp_weight(semantic_weight.unwrap_or(0.0) as f32);
    let embedding = state.embedding.clone();
    let vector_index = state.vector_index.clone();
    let initial_warnings = outcome.warnings;
    let (records, total, warnings, semantic_scores) =
        tauri::async_runtime::spawn_blocking(move || {
            execute_search(
                &db,
                &ast,
                &scope,
                weight,
                embedding,
                vector_index,
                initial_warnings,
            )
        })
        .await
        .map_err(|e| format!("検索の実行に失敗しました: {e}"))??;
    let shown = records.len() as u64;

    let (summary, generation) = {
        let mut inner = crate::catalog::lock(&state.catalog);
        let generation = inner.set_search_display(SearchDisplay {
            query_json: query_json.clone(),
            scope_root_id: scope_root,
            scope_collection_id: scope_collection,
            kind: SearchDisplayKind::Query {
                semantic_weight: weight,
            },
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
        warnings,
        query_json,
        kind: "query".to_string(),
        semantic_scores,
    })
}

/// 検索実行の本体 (キーワード = 旧経路。ハイブリッド = WP-12)。
/// 戻り値: (表示レコード, 一致総数, 警告, 意味スコア)。
#[allow(clippy::type_complexity)]
pub fn execute_search(
    db: &Arc<gento_db::Db>,
    ast: &SearchQuery,
    scope: &gento_db::SearchScope,
    weight: f32,
    embedding: Option<Arc<crate::embedding::EmbeddingService>>,
    vector_index: Option<Arc<crate::vectorindex::VectorIndexService>>,
    initial_warnings: Vec<String>,
) -> Result<
    (
        Vec<gento_core::catalog::ImageRecord>,
        u64,
        Vec<String>,
        HashMap<String, f32>,
    ),
    String,
> {
    let mut warnings = initial_warnings;
    let semantic_scores: HashMap<String, f32> = HashMap::new();
    let has_text = !ast.text_terms().is_empty();
    if weight > 0.0 && has_text {
        match (embedding, vector_index) {
            (Some(embedding), Some(vector_index)) => {
                if let Some((ordered_ids, scores)) = run_hybrid(
                    db,
                    embedding,
                    vector_index,
                    ast,
                    scope,
                    weight,
                    &mut warnings,
                ) {
                    let records = crate::collections::build_collection_records(db, &ordered_ids)
                        .map_err(|e| format!("検索結果の表示構築に失敗しました: {e}"))?;
                    let total = ordered_ids.len() as u64;
                    return Ok((records, total, warnings, scores));
                }
                // None = 警告付きフォールバック (警告はrun_hybrid内で付与済み)
            }
            _ => {
                warnings
                    .push("埋め込みサービスが利用できないため意味検索を省略しました".to_string());
            }
        }
    } else if weight > 0.0 && !has_text {
        warnings.push("検索語 (フリーテキスト) がないため意味検索を省略しました".to_string());
    }
    // 純キーワード経路 (旧WP-8動作)
    let (asset_ids, total) = collect_search_ids(db, ast, scope)?;
    let records = crate::collections::build_collection_records(db, &asset_ids)
        .map_err(|e| format!("検索結果の表示構築に失敗しました: {e}"))?;
    Ok((records, total, warnings, semantic_scores))
}

/// ハイブリッド検索 (WP-12)。意味側を実行できなかった場合はNone
/// (警告付き — 呼び出し側はキーワード経路へフォールバック)。
#[allow(clippy::type_complexity)]
fn run_hybrid(
    db: &Arc<gento_db::Db>,
    embedding: Arc<crate::embedding::EmbeddingService>,
    vector_index: Arc<crate::vectorindex::VectorIndexService>,
    ast: &SearchQuery,
    scope: &gento_db::SearchScope,
    weight: f32,
    warnings: &mut Vec<String>,
) -> Option<(Vec<i64>, HashMap<String, f32>)> {
    if !embedding.is_enabled() {
        warnings.push(
            "自然言語検索が無効のため意味検索を省略しました (AIパネルから有効化できます)"
                .to_string(),
        );
        return None;
    }
    if !vector_index.is_ready() {
        warnings.push(
            "ベクトル索引が未構築のため意味検索を省略しました (埋め込み完了後に自動構築されます)"
                .to_string(),
        );
        return None;
    }
    let query_text = ast.text_terms().join(" ");
    let query_vector = match embed_query_bounded(&embedding, &query_text, EMBED_QUERY_WAIT) {
        Ok(vector) => vector,
        Err(error) => {
            warnings.push(format!(
                "意味検索クエリの生成に失敗したため省略しました: {error}"
            ));
            return None;
        }
    };
    // 構造フィルタ (tag:/rating:等) は意味側にも適用: 対象asset集合を事前SQL
    // で絞り、USearchのlabelsフィルタ (filtered_search) で検索する
    let allowed = structural_allowed_set(db, ast, scope);
    let Some(neighbors) = vector_index.search(&query_vector, HYBRID_TOP_K, allowed.as_ref()) else {
        warnings.push(
            "索引検索に失敗したため意味検索を省略しました (索引の再構築が進行中の可能性があります)"
                .to_string(),
        );
        return None;
    };
    let semantic_pairs: Vec<(i64, f32)> = neighbors
        .iter()
        .map(|neighbor| (neighbor.key, neighbor.similarity))
        .collect();
    // キーワード側ランキング: MATCH語があればbm25順、なければ一致id昇順
    let has_match = TextPlan::compile(&ast.text_terms()).match_expr.is_some();
    let keyword_ranked: Vec<i64> = if has_match {
        db.with_reader(|repos| {
            gento_db::keyword_ranked_asset_ids(repos.conn(), ast, scope, HYBRID_TOP_K as u32)
        })
        .unwrap_or_default()
    } else {
        match collect_search_ids(db, ast, scope) {
            Ok((matched, _)) => matched.into_iter().take(HYBRID_TOP_K).collect(),
            Err(error) => {
                warnings.push(format!("キーワード側の収集に失敗しました: {error}"));
                return None;
            }
        }
    };
    let fused = weighted_rrf(&keyword_ranked, &semantic_pairs, weight);
    let ordered_ids: Vec<i64> = fused.iter().map(|item| item.asset_id).collect();
    let semantic_scores: HashMap<String, f32> = fused
        .iter()
        .filter_map(|item| {
            item.semantic_similarity
                .map(|similarity| (item.asset_id.to_string(), similarity))
        })
        .collect();
    Some((ordered_ids, semantic_scores))
}

/// モデルロード込みのクエリ埋め込み (上限待ち)。タイムアウト時もロードは
/// 続行するため、次回の検索は速くなる (§8: 準備完了後に意味検索が乗る)。
fn embed_query_bounded(
    embedding: &Arc<crate::embedding::EmbeddingService>,
    text: &str,
    timeout: Duration,
) -> Result<Vec<f32>, String> {
    let service = Arc::clone(embedding);
    let text = text.to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("hybrid-embed".into())
        .spawn(move || {
            let _ = tx.send(service.embed_query(&text));
        })
        .map_err(|e| e.to_string())?;
    match rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(_) => Err("モデルの読み込みに時間がかかっています".to_string()),
    }
}

/// 意味側に適用する構造フィルタの対象集合 (フィールド条件+スコープのみの
/// ASTで全件収集)。条件が無い (全件対象) 場合はNone (フィルタ不要)。
fn structural_allowed_set(
    db: &Arc<gento_db::Db>,
    ast: &SearchQuery,
    scope: &gento_db::SearchScope,
) -> Option<HashSet<i64>> {
    if ast.field_conditions().is_empty() && scope.root_id.is_none() && scope.collection_id.is_none()
    {
        return None;
    }
    let structural = strip_to_structural(ast);
    let (ids, _) = collect_search_ids(db, &structural, scope).ok()?;
    Some(ids.into_iter().collect())
}

/// ASTから構造条件 (フィールドterm) のみを残す (意味側フィルタ用)。
fn strip_to_structural(ast: &SearchQuery) -> SearchQuery {
    SearchQuery {
        terms: ast
            .terms
            .iter()
            .filter(|term| matches!(term, SearchTerm::Field(_)))
            .cloned()
            .collect(),
    }
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
/// async command: 全文書の内容比較をUIスレッド外で実行する (QA-WP8)。
#[tauri::command]
pub async fn fts_integrity(state: State<'_, AppState>) -> Result<FtsIntegrityDto, String> {
    let db = db_of(&state)?;
    let report = tauri::async_runtime::spawn_blocking(move || {
        db.with_reader(|repos| gento_db::fts_integrity(repos.conn()))
    })
    .await
    .map_err(|e| format!("索引診断に失敗しました: {e}"))?
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
/// async command: 修復の書き込みをUIスレッド外で実行する (QA-WP8)。
#[tauri::command]
pub async fn fts_repair(state: State<'_, AppState>) -> Result<FtsMaintenanceDto, String> {
    let db = db_of(&state)?;
    let started = Instant::now();
    let (rebuilt, removed) = tauri::async_runtime::spawn_blocking(move || {
        db.with_writer(|repos| gento_db::fts_repair(repos.conn()))
    })
    .await
    .map_err(|e| format!("索引修復に失敗しました: {e}"))?
    .map_err(|e| format!("索引修復に失敗しました: {e}"))?;
    Ok(FtsMaintenanceDto {
        rebuilt,
        removed,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

/// 現在の検索表示を新しいDB状態で再実行する (表示中の明示refresh)。
/// WP-12: 類似画像表示は同assetで再検索、ハイブリッド表示は保存済み
/// 意味重みで再実行する。
/// async command: `search_run` と同じく全件収集をUIスレッド外で実行 (QA-WP8)。
#[tauri::command]
pub async fn search_refresh(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<SearchRunDto, String> {
    let display = {
        let inner = crate::catalog::lock(&state.catalog);
        inner.search_display().cloned()
    };
    let Some(display) = display else {
        return Err("再実行できる検索がありません".to_string());
    };
    let started = Instant::now();
    let result = match display.kind {
        SearchDisplayKind::Query { semantic_weight } => {
            refresh_query(&state, &display, semantic_weight).await?
        }
        SearchDisplayKind::SimilarImage { asset_id } => refresh_similar(&state, asset_id).await?,
    };
    let (records, total, warnings, semantic_scores, kind) = result;
    let shown = records.len() as u64;
    let (summary, generation) = {
        let mut inner = crate::catalog::lock(&state.catalog);
        let generation = inner.set_search_display(SearchDisplay {
            query_json: display.query_json.clone(),
            scope_root_id: display.scope_root_id,
            scope_collection_id: display.scope_collection_id,
            kind: display.kind,
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
        warnings,
        query_json: display.query_json,
        kind,
        semantic_scores,
    })
}

/// 再実行の結果搬送 (search_refresh内部)。
type RefreshOutcome = (
    Vec<gento_core::catalog::ImageRecord>,
    u64,
    Vec<String>,
    HashMap<String, f32>,
    String,
);

async fn refresh_query(
    state: &State<'_, AppState>,
    display: &SearchDisplay,
    semantic_weight: f32,
) -> Result<RefreshOutcome, String> {
    let query = match gento_search::stored::parse_stored(&display.query_json) {
        Ok(gento_search::stored::StoredSearch::Ast(query)) => query,
        Ok(gento_search::stored::StoredSearch::Legacy(legacy)) => {
            gento_search::stored::legacy_to_query(&legacy)
        }
        Err(e) => return Err(format!("保存済み検索条件が読み込めません: {e}")),
    };
    let db = db_of(state)?;
    let scope = gento_db::SearchScope {
        root_id: display.scope_root_id,
        collection_id: display.scope_collection_id,
        require_present_files: true,
    };
    let embedding = state.embedding.clone();
    let vector_index = state.vector_index.clone();
    let (records, total, warnings, semantic_scores) =
        tauri::async_runtime::spawn_blocking(move || {
            execute_search(
                &db,
                &query,
                &scope,
                semantic_weight,
                embedding,
                vector_index,
                vec![],
            )
        })
        .await
        .map_err(|e| format!("検索の実行に失敗しました: {e}"))??;
    Ok((
        records,
        total,
        warnings,
        semantic_scores,
        "query".to_string(),
    ))
}

async fn refresh_similar(
    state: &State<'_, AppState>,
    asset_id: i64,
) -> Result<RefreshOutcome, String> {
    let db = db_of(state)?;
    let vector_index = vector_index_of(state)?;
    let (records, semantic_scores) = tauri::async_runtime::spawn_blocking(move || {
        execute_similar(&db, &vector_index, asset_id, SIMILAR_DEFAULT_K)
    })
    .await
    .map_err(|e| format!("類似画像検索に失敗しました: {e}"))??;
    let total = records.len() as u64;
    Ok((
        records,
        total,
        Vec::new(),
        semantic_scores,
        "similar".to_string(),
    ))
}

/// 類似画像検索の実行本体: 対象assetのdocベクトルで索引から近傍
/// (自己除外) を取得し、類似度順の表示レコードを組む。
pub fn execute_similar(
    db: &Arc<gento_db::Db>,
    vector_index: &Arc<crate::vectorindex::VectorIndexService>,
    asset_id: i64,
    k: usize,
) -> Result<(Vec<gento_core::catalog::ImageRecord>, HashMap<String, f32>), String> {
    let model = crate::embedding::selected_model_of(db);
    let revision = crate::embedding::model_revision_of(&model);
    let vector = db
        .with_reader(|repos| repos.embedding_vector_of_asset(asset_id, &model, &revision))
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            "この画像には埋め込みがありません (自然言語検索の処理完了後に利用できます)".to_string()
        })?;
    if !vector_index.is_ready() {
        return Err("ベクトル索引が準備中です (埋め込み完了後に自動構築されます)".to_string());
    }
    let Some(neighbors) = vector_index.search(&vector, k + 1, None) else {
        return Err("ベクトル索引が準備中です".to_string());
    };
    let mut semantic_scores: HashMap<String, f32> = HashMap::new();
    let mut ordered_ids: Vec<i64> = Vec::with_capacity(neighbors.len());
    for neighbor in neighbors {
        if neighbor.key == asset_id {
            continue;
        }
        ordered_ids.push(neighbor.key);
        semantic_scores.insert(neighbor.key.to_string(), neighbor.similarity);
    }
    ordered_ids.truncate(k);
    let records = crate::collections::build_collection_records(db, &ordered_ids)
        .map_err(|e| format!("類似画像の表示構築に失敗しました: {e}"))?;
    Ok((records, semantic_scores))
}

/// 類似画像検索 (WP-12): 対象assetのdocベクトルで近傍を検索し、
/// カタログの検索ソースとして表示する (類似度順)。
#[tauri::command]
pub async fn similar_search_run(
    app: AppHandle,
    state: State<'_, AppState>,
    asset_id: String,
    k: Option<u32>,
) -> Result<SearchRunDto, String> {
    let asset = asset_id
        .parse::<i64>()
        .map_err(|_| "画像IDが整数として解析できません".to_string())?;
    let db = db_of(&state)?;
    let vector_index = vector_index_of(&state)?;
    let started = Instant::now();
    let k = k
        .map(|value| value.clamp(1, 200) as usize)
        .unwrap_or(SIMILAR_DEFAULT_K);
    let (records, semantic_scores) =
        tauri::async_runtime::spawn_blocking(move || execute_similar(&db, &vector_index, asset, k))
            .await
            .map_err(|e| format!("類似画像検索に失敗しました: {e}"))??;
    let total = records.len() as u64;
    let shown = total;
    let (summary, generation) = {
        let mut inner = crate::catalog::lock(&state.catalog);
        // 類似画像表示のquery_jsonは空AST (一致根拠は無く、保存対象外)
        let empty_json = gento_search::SearchQuery::default().to_stored_json();
        let generation = inner.set_search_display(SearchDisplay {
            query_json: empty_json,
            scope_root_id: None,
            scope_collection_id: None,
            kind: SearchDisplayKind::SimilarImage { asset_id: asset },
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
        query_json: String::new(),
        kind: "similar".to_string(),
        semantic_scores,
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
    fn strip_to_structural_removes_text_terms() {
        let outcome = parse_query("夜景 tag:sky rating:>=3").unwrap();
        let structural = strip_to_structural(&outcome.query);
        assert!(structural.text_terms().is_empty());
        assert_eq!(structural.field_conditions().len(), 2);
        // 構造条件ゼロのクエリは空になる
        let outcome = parse_query("ただの検索語").unwrap();
        assert!(strip_to_structural(&outcome.query).terms.is_empty());
    }

    #[test]
    fn search_run_dto_serializes_camel_case_with_new_fields() {
        let dto = SearchRunDto {
            summary: CatalogSummary {
                generation: 1,
                mode: gento_core::catalog::CatalogMode::Library,
                total: 0,
                scanning: false,
                scanned_count: 0,
                error_count: 0,
                last_scan_elapsed_ms: None,
                sort: gento_core::catalog::CatalogSort::Name,
            },
            total: 3,
            shown: 3,
            elapsed_ms: 12,
            warnings: vec![],
            query_json: "{}".to_string(),
            kind: "similar".to_string(),
            semantic_scores: HashMap::from([("42".to_string(), 0.87f32)]),
        };
        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["kind"], "similar");
        let similarity = json["semanticScores"]["42"].as_f64().unwrap();
        assert!((similarity - 0.87).abs() < 1e-6, "{similarity}");
        assert!(json.get("semantic_scores").is_none(), "snake_case混入");
        assert!(json.get("kind_").is_none());
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
