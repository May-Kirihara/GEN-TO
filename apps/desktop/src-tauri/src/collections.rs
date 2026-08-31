//! コレクションcommand群 (MVP WP-6)。
//!
//! 手動/スマートコレクションの作成・編集・運用と、中央グリッドへの
//! コレクション表示を担う。実体はテスト可能な `*_impl` 関数で、
//! commandは薄いラッパー (metadata.rsと同じ構成)。
//!
//! # 設計: 明示refresh (二重反映防止)
//!
//! `asset:updated` (WP-5) が発生してもスマートコレクションの内容は
//! **自動再評価しない**。再評価は (1) `smart_collection_refresh` の明示呼び
//! 出し、(2) 次の `collection_show` (表示時評価) のいずれかでのみ行う。
//! メタデータ編集 → スマート再評価 → (将来の) 再索引化イベント…という
//! トリガー連鎖を避けるための設計 (評価は都度DBクエリで安価)。
//! 編集済み項目のバッジ等は `patch_library_assets` のin-placeパッチで
//! コレクションビューにも反映される (membershipの変化のみ明示refresh)。
//!
//! # fs無操作 (受入条件1)
//!
//! 本モジュールの全操作はSQLのみ — ファイルシステムへ触れない
//! (コレクションは仮想分類。spec §7)。
//!
//! # ガード
//!
//! stable AssetIdを受け取るcommand (項目追加・削除・並び替え・メモ) は
//! WP-5と同じモードガード (Library表示 + 仮ID拒否) を通す。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use gento_core::catalog::{ImageRecord, ImageState, ThumbnailState, Triage};
use gento_core::collections::{CollectionKind, CollectionRecord, CollectionRepo, SmartQuery};
use gento_core::library::library_asset_id;
use gento_core::repo::AssetRecord;
use gento_core::repo::{FileRecord, LibraryRootRepo, RepoError, TriageState};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::catalog::{lock, AppState};
use crate::metadata::parse_stable_asset_ids;

/// コレクションDTO (`collections_list` 等)。
/// snake_caseはWP-5委任仕様系DTOの表記規約 (ImageListItem系camelCaseとは別系統)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CollectionDto {
    /// 10進文字列 (コレクション行ID。AssetIdではない)。
    pub id: String,
    pub uuid: String,
    pub name: String,
    /// "manual" | "smart"。
    pub kind: String,
    /// smartのみ (正規形JSON)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query_json: Option<String>,
    /// manual=項目数 / smart=評価件数 (list時点)。
    pub item_count: u64,
    pub created_at: String,
    pub updated_at: String,
}

/// 手動コレクション項目DTO (`collection_items`)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CollectionItemDto {
    /// 10進文字列 (stable AssetId)。
    pub asset_id: String,
    pub position: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// `collection_items` の応答。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CollectionItemsDto {
    pub total: u64,
    pub items: Vec<CollectionItemDto>,
}

/// `smart_collection_items` の応答 (評価結果。position/noteは持たない)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SmartItemsDto {
    pub total: u64,
    /// 10進文字列 (stable AssetId。id昇順)。
    pub asset_ids: Vec<String>,
}

/// `collections:changed` イベントペイロード (左ペイン更新用)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CollectionsChangedPayload {
    /// "created" | "renamed" | "deleted" | "items" | "query" | "refreshed"。
    pub reason: String,
    /// 対象コレクションID (10進文字列。全体再取得系ではNone)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collection_id: Option<String>,
}

/// impl関数からcommand層へのイベント出力 (テストでは記録クロージャ)。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CollectionEvent {
    /// `collections:changed`。
    Changed(CollectionsChangedPayload),
    /// `catalog:changed` (collection表示の切替/再評価)。refresh=trueは
    /// CollectionSwitchではなくCollectionRefresh理由でemitする。
    CatalogChanged { generation: u64, refresh: bool },
}

/// コレクションの評価・表示上限 (全件取得系の安全上限。MVPの運用規模に対し
/// 十分大きい値 — 5万件カタログ想定を1ビューで保持する既存設計と同程度)。
const FULL_VIEW_LIMIT: u32 = u32::MAX / 2;

fn db_of(state: &AppState) -> Result<Arc<gento_db::Db>, String> {
    state
        .db
        .as_ref()
        .and_then(|d| d.db.clone())
        .ok_or_else(|| "データベースが利用できないためコレクションを操作できません".to_string())
}

fn repo_error_message(err: RepoError) -> String {
    match err {
        RepoError::NotFound => "対象のコレクション (または画像) が見つかりません".to_string(),
        RepoError::Constraint(msg) => format!("コレクションの操作を受け付けられません: {msg}"),
        other => format!("データベースエラー: {other}"),
    }
}

fn parse_collection_id(value: &str) -> Result<i64, String> {
    value
        .parse()
        .map_err(|_| format!("無効なコレクションIDです: {value}"))
}

fn collection_dto(record: &CollectionRecord) -> CollectionDto {
    CollectionDto {
        id: record.id.to_string(),
        uuid: record.uuid.clone(),
        name: record.name.clone(),
        kind: record.kind.as_str().to_string(),
        query_json: record.query_json.clone(),
        item_count: record.item_count,
        created_at: record.created_at.clone(),
        updated_at: record.updated_at.clone(),
    }
}

fn parse_smart_query(json: &str) -> Result<SmartQuery, String> {
    SmartQuery::parse(json).map_err(|e| e.0)
}

/// コレクションのasset行ID列 (manual=position順全件 / smart=評価全件)。
fn collection_asset_ids(db: &gento_db::Db, record: &CollectionRecord) -> Result<Vec<i64>, String> {
    let ids = match record.kind {
        CollectionKind::Manual => {
            let (items, total) = db
                .with_reader(|repos| repos.list_collection_items(record.id, 0, FULL_VIEW_LIMIT))
                .map_err(repo_error_message)?;
            debug_assert_eq!(items.len() as u64, total);
            items.into_iter().map(|item| item.asset_id).collect()
        }
        CollectionKind::Smart => {
            let (ids, total) = db
                .with_reader(|repos| repos.evaluate_smart_collection(record.id, 0, FULL_VIEW_LIMIT))
                .map_err(repo_error_message)?;
            debug_assert_eq!(ids.len() as u64, total);
            ids
        }
    };
    Ok(ids)
}

/// 表示レコード構築 (files JOIN assets + ルートパス解決)。
/// `asset_ids` の順序を保持し、presentなFileを持たないassetはスキップする。
pub fn build_collection_records(
    db: &gento_db::Db,
    asset_ids: &[i64],
) -> Result<Vec<ImageRecord>, RepoError> {
    let root_paths: HashMap<i64, PathBuf> = db
        .with_reader(|repos| repos.list_roots())?
        .into_iter()
        .map(|root| (root.id, root.path))
        .collect();
    let joined = db.with_reader(|repos| repos.list_present_files_of_assets(asset_ids))?;
    let mut records = Vec::with_capacity(joined.len());
    for (file, asset) in joined {
        let Some(root_path) = root_paths.get(&file.root_id) else {
            continue;
        };
        records.push(image_record_of(root_path, &file, &asset));
    }
    Ok(records)
}

fn image_record_of(
    root_path: &std::path::Path,
    file: &FileRecord,
    asset: &AssetRecord,
) -> ImageRecord {
    ImageRecord {
        id: library_asset_id(asset.id),
        source_path: root_path.join(&file.rel_path),
        display_name: file.display_name.clone(),
        width: asset.width,
        height: asset.height,
        byte_size: file.byte_size,
        mtime_ms: file.mtime_ms,
        mime_type: asset.mime_type.clone(),
        is_animated: asset.is_animated.unwrap_or(false),
        state: ImageState::Ready,
        thumb_state: ThumbnailState::Missing,
        rating: asset.rating,
        triage: match asset.triage {
            TriageState::Unselected => Triage::Undecided,
            TriageState::Picked => Triage::Keep,
            TriageState::Rejected => Triage::Reject,
        },
        favorite: asset.favorite,
    }
}

/// 表示中コレクションを再構築して表示へ反映する (generation進行 + catalog:changed)。
/// 表示中でない場合は何もしない。`refresh` はイベント理由の切替 (初回表示=false)。
fn refresh_display_if_showing(
    state: &AppState,
    emit: &dyn Fn(&CollectionEvent),
    db: &gento_db::Db,
    collection_id: i64,
    refresh: bool,
) -> Result<(), String> {
    let showing = lock(&state.catalog).collection_view()
        == Some(crate::catalog::CollectionView { collection_id });
    if !showing {
        return Ok(());
    }
    let record = db
        .with_reader(|repos| repos.get_collection(collection_id))
        .map_err(repo_error_message)?
        .ok_or_else(|| "対象のコレクション (または画像) が見つかりません".to_string())?;
    let asset_ids = collection_asset_ids(db, &record)?;
    let records = build_collection_records(db, &asset_ids).map_err(repo_error_message)?;
    let generation = lock(&state.catalog)
        .set_collection_display(crate::catalog::CollectionView { collection_id }, records);
    emit(&CollectionEvent::CatalogChanged {
        generation,
        refresh,
    });
    Ok(())
}

// ===== 実装本体 (テストから直接呼ぶ) =====

pub(crate) fn collections_list_impl(state: &AppState) -> Result<Vec<CollectionDto>, String> {
    let db = db_of(state)?;
    db.with_reader(|repos| repos.list_collections())
        .map(|records| records.iter().map(collection_dto).collect())
        .map_err(repo_error_message)
}

pub(crate) fn collection_create_impl(
    state: &AppState,
    emit: &dyn Fn(&CollectionEvent),
    name: String,
) -> Result<CollectionDto, String> {
    let db = db_of(state)?;
    let record = db
        .with_writer(|repos| repos.create_manual_collection(&name))
        .map_err(repo_error_message)?;
    let dto = collection_dto(&record);
    emit(&CollectionEvent::Changed(CollectionsChangedPayload {
        reason: "created".to_string(),
        collection_id: Some(dto.id.clone()),
    }));
    Ok(dto)
}

pub(crate) fn smart_collection_create_impl(
    state: &AppState,
    emit: &dyn Fn(&CollectionEvent),
    name: String,
    query_json: String,
    search_json: Option<String>,
) -> Result<CollectionDto, String> {
    let db = db_of(state)?;
    // WP-8: search_json (検索AST・version 2) があればそちらを正本とする。
    // 旧query_json (最小条件) は既存呼び出し側 (E2E・旧UI) の互換用。
    let record = match search_json {
        Some(ast_json) => db
            .with_writer(|repos| repos.create_smart_collection_search(&name, &ast_json))
            .map_err(repo_error_message)?,
        None => {
            let query = parse_smart_query(&query_json)?;
            db.with_writer(|repos| repos.create_smart_collection(&name, &query))
                .map_err(repo_error_message)?
        }
    };
    let dto = collection_dto(&record);
    emit(&CollectionEvent::Changed(CollectionsChangedPayload {
        reason: "created".to_string(),
        collection_id: Some(dto.id.clone()),
    }));
    Ok(dto)
}

pub(crate) fn collection_rename_impl(
    state: &AppState,
    emit: &dyn Fn(&CollectionEvent),
    collection_id: String,
    name: String,
) -> Result<CollectionDto, String> {
    let id = parse_collection_id(&collection_id)?;
    let db = db_of(state)?;
    let record = db
        .with_writer(|repos| repos.rename_collection(id, &name))
        .map_err(repo_error_message)?;
    let dto = collection_dto(&record);
    emit(&CollectionEvent::Changed(CollectionsChangedPayload {
        reason: "renamed".to_string(),
        collection_id: Some(dto.id.clone()),
    }));
    Ok(dto)
}

pub(crate) fn smart_collection_update_impl(
    state: &AppState,
    emit: &dyn Fn(&CollectionEvent),
    collection_id: String,
    name: String,
    query_json: String,
) -> Result<CollectionDto, String> {
    let id = parse_collection_id(&collection_id)?;
    let query = parse_smart_query(&query_json)?;
    let db = db_of(state)?;
    let record = db
        .with_writer(|repos| repos.update_smart_collection(id, &name, &query))
        .map_err(repo_error_message)?;
    let dto = collection_dto(&record);
    // 条件が変わったら表示中ビューも追従 (明示更新操作のためrefreshでよい)
    refresh_display_if_showing(state, emit, &db, id, true)?;
    emit(&CollectionEvent::Changed(CollectionsChangedPayload {
        reason: "query".to_string(),
        collection_id: Some(dto.id.clone()),
    }));
    Ok(dto)
}

pub(crate) fn collection_delete_impl(
    state: &AppState,
    emit: &dyn Fn(&CollectionEvent),
    collection_id: String,
) -> Result<(), String> {
    let id = parse_collection_id(&collection_id)?;
    let db = db_of(state)?;
    db.with_writer(|repos| repos.delete_collection(id))
        .map_err(repo_error_message)?;
    // 表示中だった場合は通常ライブラリ表示へ戻す
    let generation = lock(&state.catalog).clear_collection_display();
    if generation > 0 {
        emit(&CollectionEvent::CatalogChanged {
            generation,
            refresh: false,
        });
    }
    emit(&CollectionEvent::Changed(CollectionsChangedPayload {
        reason: "deleted".to_string(),
        collection_id: Some(collection_id),
    }));
    Ok(())
}

pub(crate) fn collection_add_items_impl(
    state: &AppState,
    emit: &dyn Fn(&CollectionEvent),
    collection_id: String,
    ids: Vec<String>,
) -> Result<(), String> {
    let id = parse_collection_id(&collection_id)?;
    let asset_ids = parse_stable_asset_ids(crate::metadata::current_mode(state), &ids)?;
    let db = db_of(state)?;
    db.with_writer(|repos| repos.add_collection_items(id, &asset_ids))
        .map_err(repo_error_message)?;
    refresh_display_if_showing(state, emit, &db, id, true)?;
    emit(&CollectionEvent::Changed(CollectionsChangedPayload {
        reason: "items".to_string(),
        collection_id: Some(collection_id),
    }));
    Ok(())
}

pub(crate) fn collection_remove_item_impl(
    state: &AppState,
    emit: &dyn Fn(&CollectionEvent),
    collection_id: String,
    asset_id: String,
) -> Result<(), String> {
    let id = parse_collection_id(&collection_id)?;
    let asset = parse_stable_asset_ids(crate::metadata::current_mode(state), &[asset_id])?[0];
    let db = db_of(state)?;
    db.with_writer(|repos| repos.remove_collection_item(id, asset))
        .map_err(repo_error_message)?;
    refresh_display_if_showing(state, emit, &db, id, true)?;
    emit(&CollectionEvent::Changed(CollectionsChangedPayload {
        reason: "items".to_string(),
        collection_id: Some(collection_id),
    }));
    Ok(())
}

pub(crate) fn collection_reorder_impl(
    state: &AppState,
    emit: &dyn Fn(&CollectionEvent),
    collection_id: String,
    asset_ids: Vec<String>,
) -> Result<(), String> {
    let id = parse_collection_id(&collection_id)?;
    let order = parse_stable_asset_ids(crate::metadata::current_mode(state), &asset_ids)?;
    let db = db_of(state)?;
    db.with_writer(|repos| repos.reorder_collection(id, &order))
        .map_err(repo_error_message)?;
    refresh_display_if_showing(state, emit, &db, id, true)?;
    emit(&CollectionEvent::Changed(CollectionsChangedPayload {
        reason: "items".to_string(),
        collection_id: Some(collection_id),
    }));
    Ok(())
}

pub(crate) fn collection_items_impl(
    state: &AppState,
    collection_id: String,
    offset: Option<u32>,
    limit: Option<u32>,
) -> Result<CollectionItemsDto, String> {
    let id = parse_collection_id(&collection_id)?;
    let offset = offset.unwrap_or(0);
    let limit = limit.unwrap_or(200).clamp(1, 1000);
    let db = db_of(state)?;
    let (items, total) = db
        .with_reader(|repos| repos.list_collection_items(id, offset, limit))
        .map_err(repo_error_message)?;
    Ok(CollectionItemsDto {
        total,
        items: items
            .into_iter()
            .map(|item| CollectionItemDto {
                asset_id: item.asset_id.to_string(),
                position: item.position,
                note: item.note,
            })
            .collect(),
    })
}

pub(crate) fn collection_set_item_note_impl(
    state: &AppState,
    emit: &dyn Fn(&CollectionEvent),
    collection_id: String,
    asset_id: String,
    note: Option<String>,
) -> Result<CollectionItemDto, String> {
    let id = parse_collection_id(&collection_id)?;
    let asset = parse_stable_asset_ids(crate::metadata::current_mode(state), &[asset_id])?[0];
    let db = db_of(state)?;
    let record = db
        .with_writer(|repos| repos.set_collection_item_note(id, asset, note.as_deref()))
        .map_err(repo_error_message)?;
    emit(&CollectionEvent::Changed(CollectionsChangedPayload {
        reason: "items".to_string(),
        collection_id: Some(collection_id),
    }));
    Ok(CollectionItemDto {
        asset_id: record.asset_id.to_string(),
        position: record.position,
        note: record.note,
    })
}

pub(crate) fn smart_collection_items_impl(
    state: &AppState,
    collection_id: String,
    offset: Option<u32>,
    limit: Option<u32>,
) -> Result<SmartItemsDto, String> {
    let id = parse_collection_id(&collection_id)?;
    let offset = offset.unwrap_or(0);
    let limit = limit.unwrap_or(200).clamp(1, 1000);
    let db = db_of(state)?;
    let (ids, total) = db
        .with_reader(|repos| repos.evaluate_smart_collection(id, offset, limit))
        .map_err(repo_error_message)?;
    Ok(SmartItemsDto {
        total,
        asset_ids: ids.into_iter().map(|id| id.to_string()).collect(),
    })
}

pub(crate) fn smart_collection_refresh_impl(
    state: &AppState,
    emit: &dyn Fn(&CollectionEvent),
    collection_id: String,
) -> Result<CollectionDto, String> {
    let id = parse_collection_id(&collection_id)?;
    let db = db_of(state)?;
    // 評価は readクエリ (件数確定のためget_collectionが評価を行う)
    let record = db
        .with_reader(|repos| repos.get_collection(id))
        .map_err(repo_error_message)?
        .ok_or_else(|| "対象のコレクション (または画像) が見つかりません".to_string())?;
    if record.kind != CollectionKind::Smart {
        return Err("手動コレクションは再評価の対象外です".to_string());
    }
    refresh_display_if_showing(state, emit, &db, id, true)?;
    let dto = collection_dto(&record);
    emit(&CollectionEvent::Changed(CollectionsChangedPayload {
        reason: "refreshed".to_string(),
        collection_id: Some(dto.id.clone()),
    }));
    Ok(dto)
}

/// コレクションを中央グリッドへ表示する (manual=position順 / smart=評価結果)。
/// Libraryモードへ遷移し、generationを進めて `catalog:changed` をemitする。
pub(crate) fn collection_show_impl(
    state: &AppState,
    emit: &dyn Fn(&CollectionEvent),
    collection_id: String,
) -> Result<crate::catalog::CatalogSummary, String> {
    let id = parse_collection_id(&collection_id)?;
    let db = db_of(state)?;
    let record = db
        .with_reader(|repos| repos.get_collection(id))
        .map_err(repo_error_message)?
        .ok_or_else(|| "対象のコレクション (または画像) が見つかりません".to_string())?;
    let asset_ids = collection_asset_ids(&db, &record)?;
    let records = build_collection_records(&db, &asset_ids).map_err(repo_error_message)?;
    let generation = lock(&state.catalog).set_collection_display(
        crate::catalog::CollectionView { collection_id: id },
        records,
    );
    let summary = crate::catalog::summary_of_pub(&lock(&state.catalog));
    emit(&CollectionEvent::CatalogChanged {
        generation,
        refresh: false,
    });
    Ok(summary)
}

// ===== Tauri command (薄ラッパ) =====

fn tauri_emitter<'a>(app: &'a AppHandle) -> impl Fn(&CollectionEvent) + use<'a> {
    move |event| match event {
        CollectionEvent::Changed(payload) => {
            let _ = app.emit("collections:changed", payload);
        }
        CollectionEvent::CatalogChanged {
            generation,
            refresh,
        } => {
            let _ = app.emit(
                "catalog:changed",
                crate::catalog::CatalogChangedPayload {
                    generation: *generation,
                    reason: if *refresh {
                        crate::catalog::CatalogChangeReason::CollectionRefresh
                    } else {
                        crate::catalog::CatalogChangeReason::CollectionSwitch
                    },
                },
            );
        }
    }
}

#[tauri::command]
pub fn collections_list(state: State<'_, AppState>) -> Result<Vec<CollectionDto>, String> {
    collections_list_impl(&state)
}

#[tauri::command]
pub fn collection_create(
    app: AppHandle,
    state: State<'_, AppState>,
    name: String,
) -> Result<CollectionDto, String> {
    collection_create_impl(&state, &tauri_emitter(&app), name)
}

#[tauri::command]
pub fn collection_rename(
    app: AppHandle,
    state: State<'_, AppState>,
    collection_id: String,
    name: String,
) -> Result<CollectionDto, String> {
    collection_rename_impl(&state, &tauri_emitter(&app), collection_id, name)
}

#[tauri::command]
pub fn collection_delete(
    app: AppHandle,
    state: State<'_, AppState>,
    collection_id: String,
) -> Result<(), String> {
    collection_delete_impl(&state, &tauri_emitter(&app), collection_id)
}

#[tauri::command]
pub fn collection_add_items(
    app: AppHandle,
    state: State<'_, AppState>,
    collection_id: String,
    ids: Vec<String>,
) -> Result<(), String> {
    collection_add_items_impl(&state, &tauri_emitter(&app), collection_id, ids)
}

#[tauri::command]
pub fn collection_remove_item(
    app: AppHandle,
    state: State<'_, AppState>,
    collection_id: String,
    asset_id: String,
) -> Result<(), String> {
    collection_remove_item_impl(&state, &tauri_emitter(&app), collection_id, asset_id)
}

#[tauri::command]
pub fn collection_reorder(
    app: AppHandle,
    state: State<'_, AppState>,
    collection_id: String,
    asset_ids: Vec<String>,
) -> Result<(), String> {
    collection_reorder_impl(&state, &tauri_emitter(&app), collection_id, asset_ids)
}

#[tauri::command]
pub fn collection_items(
    state: State<'_, AppState>,
    collection_id: String,
    offset: Option<u32>,
    limit: Option<u32>,
) -> Result<CollectionItemsDto, String> {
    collection_items_impl(&state, collection_id, offset, limit)
}

#[tauri::command]
pub fn collection_set_item_note(
    app: AppHandle,
    state: State<'_, AppState>,
    collection_id: String,
    asset_id: String,
    note: Option<String>,
) -> Result<CollectionItemDto, String> {
    collection_set_item_note_impl(&state, &tauri_emitter(&app), collection_id, asset_id, note)
}

#[tauri::command]
pub fn smart_collection_create(
    app: AppHandle,
    state: State<'_, AppState>,
    name: String,
    query_json: String,
    search_json: Option<String>,
) -> Result<CollectionDto, String> {
    smart_collection_create_impl(&state, &tauri_emitter(&app), name, query_json, search_json)
}

#[tauri::command]
pub fn smart_collection_update(
    app: AppHandle,
    state: State<'_, AppState>,
    collection_id: String,
    name: String,
    query_json: String,
) -> Result<CollectionDto, String> {
    smart_collection_update_impl(
        &state,
        &tauri_emitter(&app),
        collection_id,
        name,
        query_json,
    )
}

#[tauri::command]
pub fn smart_collection_items(
    state: State<'_, AppState>,
    collection_id: String,
    offset: Option<u32>,
    limit: Option<u32>,
) -> Result<SmartItemsDto, String> {
    smart_collection_items_impl(&state, collection_id, offset, limit)
}

#[tauri::command]
pub fn smart_collection_refresh(
    app: AppHandle,
    state: State<'_, AppState>,
    collection_id: String,
) -> Result<CollectionDto, String> {
    smart_collection_refresh_impl(&state, &tauri_emitter(&app), collection_id)
}

#[tauri::command]
pub fn collection_show(
    app: AppHandle,
    state: State<'_, AppState>,
    collection_id: String,
) -> Result<crate::catalog::CatalogSummary, String> {
    collection_show_impl(&state, &tauri_emitter(&app), collection_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::AppState;
    use gento_core::catalog::{CatalogMode, CatalogSort};
    use gento_core::repo::{AssetRepo, FileRepo, FileUpsert, LibraryRootRepo, NewAsset, ScanState};
    use std::path::{Path, PathBuf};
    use std::sync::Mutex as StdMutex;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "gento-tauri-coll-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Libraryモード + テストDB + 登録ルート + asset群 を持つAppState。
    struct TestApp {
        _tmp: TempDir,
        state: AppState,
        events: StdMutex<Vec<CollectionEvent>>,
    }

    impl TestApp {
        fn new(tag: &str) -> Self {
            let tmp = TempDir::new(tag);
            let db = gento_db::Db::open(tmp.0.join("db.sqlite3")).unwrap();
            let mut state = AppState::new(PathBuf::from("/tmp/gento-coll-dataset"));
            state.db = Some(crate::db::DbState {
                path: tmp.0.join("db.sqlite3"),
                db: Some(std::sync::Arc::new(db)),
                open_error: None,
            });
            {
                let mut inner = lock(&state.catalog);
                inner.force_mode_for_test(CatalogMode::Library);
            }
            let events = StdMutex::new(Vec::new());
            Self {
                _tmp: tmp,
                state,
                events,
            }
        }

        fn db(&self) -> &gento_db::Db {
            self.state.db.as_ref().unwrap().db.as_ref().unwrap()
        }

        fn emit(&self, event: &CollectionEvent) {
            self.events.lock().unwrap().push(event.clone());
        }

        /// 登録ルート + presentファイル付きassetを作り、行IDを返す。
        fn seed_rooted_asset(&self, hash: &str, rel: &str) -> i64 {
            self.db()
                .with_writer(|repos| {
                    let root = match repos.list_roots()?.first() {
                        Some(root) => root.clone(),
                        None => repos.add_root(Path::new("/data/roots/x"))?,
                    };
                    let asset = repos
                        .create_asset(NewAsset {
                            content_hash: format!("blake3:{hash}"),
                            hash_algorithm: "blake3".to_string(),
                            byte_size: 10,
                            mime_type: Some("image/png".to_string()),
                            width: Some(10),
                            height: Some(20),
                            has_alpha: None,
                            is_animated: None,
                            frame_count: None,
                        })?
                        .clone();
                    let file = repos.upsert_file(FileUpsert {
                        root_id: root.id,
                        rel_path: PathBuf::from(rel),
                        display_name: rel.to_string(),
                        byte_size: 10,
                        mtime_ms: 1,
                        mtime_ns: 1_000,
                        scan_state: ScanState::Present,
                        os_file_id: None,
                        detach_asset: false,
                    })?;
                    repos.link_file(file.id, asset.id)?;
                    Ok(asset.id)
                })
                .unwrap()
        }

        fn events_of_kind(&self, f: impl Fn(&CollectionEvent) -> bool) -> Vec<CollectionEvent> {
            self.events
                .lock()
                .unwrap()
                .iter()
                .filter(|e| f(e))
                .cloned()
                .collect()
        }
    }

    const RATING_QUERY: &str = r#"{"conditions":[{"type":"rating_ge","value":4}]}"#;

    #[test]
    fn manual_command_flow_create_add_reorder_show_note_remove_delete() {
        let app = TestApp::new("flow");
        let a1 = app.seed_rooted_asset("f1", "a1.png");
        let a2 = app.seed_rooted_asset("f2", "a2.png");
        let a3 = app.seed_rooted_asset("f3", "a3.png");

        // 作成
        let created =
            collection_create_impl(&app.state, &|e| app.emit(e), " 選択 ".into()).unwrap();
        assert_eq!(created.name, "選択");
        assert_eq!(created.kind, "manual");
        assert_eq!(created.item_count, 0);
        let cid = created.id.clone();

        // 追加 (stable AssetIdは10進文字列)
        collection_add_items_impl(
            &app.state,
            &|e| app.emit(e),
            cid.clone(),
            vec![a1.to_string(), a2.to_string(), a3.to_string()],
        )
        .unwrap();

        // 一覧・件数
        let list = collections_list_impl(&app.state).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].item_count, 3);

        // 項目一覧 (position順)
        let items = collection_items_impl(&app.state, cid.clone(), None, None).unwrap();
        assert_eq!(items.total, 3);
        assert_eq!(
            items
                .items
                .iter()
                .map(|i| i.asset_id.as_str())
                .collect::<Vec<_>>(),
            vec![
                a1.to_string().as_str(),
                a2.to_string().as_str(),
                a3.to_string().as_str()
            ]
        );

        // 並び替え (a3を先頭へ)
        collection_reorder_impl(
            &app.state,
            &|e| app.emit(e),
            cid.clone(),
            vec![a3.to_string(), a1.to_string(), a2.to_string()],
        )
        .unwrap();

        // 表示: summaryがposition順の全件数・Libraryモード
        let summary = collection_show_impl(&app.state, &|e| app.emit(e), cid.clone()).unwrap();
        assert_eq!(summary.mode, CatalogMode::Library);
        assert_eq!(summary.total, 3);
        {
            let inner = lock(&app.state.catalog);
            assert_eq!(
                inner
                    .records()
                    .iter()
                    .map(|r| r.id.value().to_string())
                    .collect::<Vec<_>>(),
                vec![a3.to_string(), a1.to_string(), a2.to_string()],
                "manual表示はposition順"
            );
            assert_eq!(
                inner.collection_view().map(|v| v.collection_id.to_string()),
                Some(cid.clone())
            );
        }

        // メモ設定 (表示中の項目)
        let note = collection_set_item_note_impl(
            &app.state,
            &|e| app.emit(e),
            cid.clone(),
            a1.to_string(),
            Some("ベストショット".to_string()),
        )
        .unwrap();
        assert_eq!(note.note.as_deref(), Some("ベストショット"));
        let items = collection_items_impl(&app.state, cid.clone(), None, None).unwrap();
        assert_eq!(
            items
                .items
                .iter()
                .find(|i| i.asset_id == a1.to_string())
                .unwrap()
                .note,
            Some("ベストショット".to_string())
        );

        // 削除 → 詰め + 表示更新 (refresh)
        collection_remove_item_impl(&app.state, &|e| app.emit(e), cid.clone(), a3.to_string())
            .unwrap();
        {
            let inner = lock(&app.state.catalog);
            assert_eq!(inner.records().len(), 2, "表示から即座に消える");
            assert_eq!(inner.records()[0].id.value() as i64, a1);
        }

        // 改名
        let renamed =
            collection_rename_impl(&app.state, &|e| app.emit(e), cid.clone(), "best".into())
                .unwrap();
        assert_eq!(renamed.name, "best");

        // 削除 → 通常ライブラリ表示へ戻る
        collection_delete_impl(&app.state, &|e| app.emit(e), cid.clone()).unwrap();
        assert!(collections_list_impl(&app.state).unwrap().is_empty());
        {
            let inner = lock(&app.state.catalog);
            assert!(inner.collection_view().is_none());
        }
        let err = collection_show_impl(&app.state, &|e| app.emit(e), cid.clone()).unwrap_err();
        assert!(err.contains("見つかりません"), "{err}");

        // collections:changedが各操作で送出されている
        let reasons: Vec<String> = app
            .events_of_kind(|e| matches!(e, CollectionEvent::Changed(_)))
            .into_iter()
            .filter_map(|e| match e {
                CollectionEvent::Changed(payload) => Some(payload.reason),
                _ => None,
            })
            .collect();
        assert_eq!(
            reasons,
            vec!["created", "items", "items", "items", "items", "renamed", "deleted"]
        );
        // catalog:changed (show・refresh・delete由来)
        let catalog_events =
            app.events_of_kind(|e| matches!(e, CollectionEvent::CatalogChanged { .. }));
        assert!(catalog_events.len() >= 3);
    }

    #[test]
    fn adhoc_and_provisional_ids_rejected() {
        let app = TestApp::new("adhoc");
        let a1 = app.seed_rooted_asset("ah1", "a.png");
        let created = collection_create_impl(&app.state, &|e| app.emit(e), "m".into()).unwrap();
        // Real (一時走査) モードへ戻す
        {
            let mut inner = lock(&app.state.catalog);
            inner.force_mode_for_test(CatalogMode::Real);
        }
        let err = collection_add_items_impl(
            &app.state,
            &|e| app.emit(e),
            created.id.clone(),
            vec![a1.to_string()],
        )
        .unwrap_err();
        assert!(err.contains("未登録フォルダ"), "{err}");
        let err = collection_reorder_impl(
            &app.state,
            &|e| app.emit(e),
            created.id.clone(),
            vec![a1.to_string()],
        )
        .unwrap_err();
        assert!(err.contains("未登録フォルダ"), "{err}");
        let err = collection_set_item_note_impl(
            &app.state,
            &|e| app.emit(e),
            created.id.clone(),
            a1.to_string(),
            Some("x".to_string()),
        )
        .unwrap_err();
        assert!(err.contains("未登録フォルダ"), "{err}");
        // Libraryモードでも仮ID (bit63) は拒否
        {
            let mut inner = lock(&app.state.catalog);
            inner.force_mode_for_test(CatalogMode::Library);
        }
        let provisional = format!("{}", (1u64 << 63) | 5);
        let err = collection_add_items_impl(
            &app.state,
            &|e| app.emit(e),
            created.id.clone(),
            vec![provisional],
        )
        .unwrap_err();
        assert!(err.contains("ハッシュ計算待ち"), "{err}");
        // 無効なコレクションID
        let err = collection_rename_impl(&app.state, &|e| app.emit(e), "abc".into(), "x".into())
            .unwrap_err();
        assert!(err.contains("無効なコレクションID"), "{err}");
    }

    #[test]
    fn smart_flow_items_show_and_explicit_refresh() {
        let app = TestApp::new("smart");
        let low = app.seed_rooted_asset("s1", "low.png");
        let high = app.seed_rooted_asset("s2", "high.png");
        app.db()
            .with_writer(|repos| repos.set_rating(high, 5))
            .unwrap();

        // 作成 (query_json文字列 → 厳密検証)
        let created = smart_collection_create_impl(
            &app.state,
            &|e| app.emit(e),
            "高評価".into(),
            RATING_QUERY.to_string(),
            None,
        )
        .unwrap();
        assert_eq!(created.kind, "smart");
        assert_eq!(created.item_count, 1);
        assert_eq!(created.query_json.as_deref(), Some(RATING_QUERY));

        // 評価結果一覧
        let items =
            smart_collection_items_impl(&app.state, created.id.clone(), None, None).unwrap();
        assert_eq!(items.total, 1);
        assert_eq!(items.asset_ids, vec![high.to_string()]);

        // 表示
        let summary =
            collection_show_impl(&app.state, &|e| app.emit(e), created.id.clone()).unwrap();
        assert_eq!(summary.total, 1);
        {
            let inner = lock(&app.state.catalog);
            assert_eq!(inner.records()[0].id.value() as i64, high);
            assert_eq!(inner.records()[0].rating, 5, "評価値がレコードへ載る");
        }

        // メタデータ変更 (WP-5経路相当: repoでrating変更) — 表示は自動再評価しない
        app.db()
            .with_writer(|repos| repos.set_rating(low, 4))
            .unwrap();
        {
            let inner = lock(&app.state.catalog);
            assert_eq!(inner.records().len(), 1, "明示refreshまで表示は変化しない");
        }

        // 明示refresh → 表示が再評価される (受入条件3のcommand層証明)
        let refreshed =
            smart_collection_refresh_impl(&app.state, &|e| app.emit(e), created.id.clone())
                .unwrap();
        assert_eq!(refreshed.item_count, 2);
        {
            let inner = lock(&app.state.catalog);
            assert_eq!(inner.records().len(), 2);
            let ids: Vec<i64> = inner
                .records()
                .iter()
                .map(|r| r.id.value() as i64)
                .collect();
            assert_eq!(ids, vec![low, high], "評価順 (id昇順)");
        }
        // refreshはcatalog:changed (CollectionRefresh) を出す
        let refresh_events = app
            .events_of_kind(|e| matches!(e, CollectionEvent::CatalogChanged { refresh: true, .. }));
        assert_eq!(refresh_events.len(), 1);

        // smart更新 (query変更で表示も追従)
        let updated = smart_collection_update_impl(
            &app.state,
            &|e| app.emit(e),
            created.id.clone(),
            "除外候補".into(),
            r#"{"conditions":[{"type":"cull_state","value":"rejected"}]}"#.to_string(),
        )
        .unwrap();
        assert_eq!(updated.name, "除外候補");
        assert_eq!(updated.item_count, 0);
        {
            let inner = lock(&app.state.catalog);
            assert!(inner.records().is_empty(), "query更新で表示が空になる");
        }

        // 手動コレクションへのsmart操作は拒否
        let manual = collection_create_impl(&app.state, &|e| app.emit(e), "m".into()).unwrap();
        let err = smart_collection_update_impl(
            &app.state,
            &|e| app.emit(e),
            manual.id.clone(),
            "x".into(),
            RATING_QUERY.to_string(),
        )
        .unwrap_err();
        assert!(err.contains("受け付けられません"), "{err}");
        let err = smart_collection_refresh_impl(&app.state, &|e| app.emit(e), manual.id.clone())
            .unwrap_err();
        assert!(err.contains("対象外"), "{err}");
    }

    #[test]
    fn smart_invalid_query_json_rejected_in_japanese() {
        let app = TestApp::new("invalid");
        for bad in [
            "not json",
            "{}",
            r#"{"conditions":[{"type":"unknown","value":1}]}"#,
            r#"{"conditions":[{"type":"rating_ge","value":9}]}"#,
        ] {
            let err = smart_collection_create_impl(
                &app.state,
                &|e| app.emit(e),
                "s".into(),
                bad.to_string(),
                None,
            )
            .unwrap_err();
            assert!(!err.is_empty(), "拒否: {bad}");
        }
        // 空名も拒否
        let err = smart_collection_create_impl(
            &app.state,
            &|e| app.emit(e),
            "   ".into(),
            RATING_QUERY.to_string(),
            None,
        )
        .unwrap_err();
        assert!(err.contains("受け付けられません"), "{err}");
        assert!(collections_list_impl(&app.state).unwrap().is_empty());
    }

    #[test]
    fn sort_rejected_while_collection_displayed() {
        let app = TestApp::new("sortguard");
        let a1 = app.seed_rooted_asset("sg1", "a.png");
        let created = collection_create_impl(&app.state, &|e| app.emit(e), "m".into()).unwrap();
        collection_add_items_impl(
            &app.state,
            &|e| app.emit(e),
            created.id.clone(),
            vec![a1.to_string()],
        )
        .unwrap();
        collection_show_impl(&app.state, &|e| app.emit(e), created.id.clone()).unwrap();
        // ソート変更ガード (apply_sort_change = set_catalog_sort本体)
        let err = {
            let mut inner = lock(&app.state.catalog);
            crate::catalog::apply_sort_change(&mut inner, CatalogSort::DateDesc).map(|_| ())
        }
        .unwrap_err();
        assert!(err.contains("コレクション表示中"), "{err}");
    }

    #[test]
    fn collection_records_skip_assets_without_present_file() {
        let app = TestApp::new("nofile");
        // presentなfileを持たないasset
        let a_nofile = app
            .db()
            .with_writer(|repos| {
                repos
                    .create_asset(NewAsset {
                        content_hash: "blake3:nf".to_string(),
                        hash_algorithm: "blake3".to_string(),
                        byte_size: 1,
                        mime_type: None,
                        width: None,
                        height: None,
                        has_alpha: None,
                        is_animated: None,
                        frame_count: None,
                    })
                    .map(|a| a.id)
            })
            .unwrap();
        let a_file = app.seed_rooted_asset("wf", "ok.png");
        let created = collection_create_impl(&app.state, &|e| app.emit(e), "m".into()).unwrap();
        collection_add_items_impl(
            &app.state,
            &|e| app.emit(e),
            created.id.clone(),
            vec![a_nofile.to_string(), a_file.to_string()],
        )
        .unwrap();
        let summary =
            collection_show_impl(&app.state, &|e| app.emit(e), created.id.clone()).unwrap();
        assert_eq!(summary.total, 1, "表示できないassetはビューから除外");
        assert_eq!(
            collections_list_impl(&app.state).unwrap()[0].item_count,
            2,
            "件数 (項目数) は保持される"
        );
    }

    /// WP-6 DTOの直列化形状 (snake_case)。フロントの型定義 (lib/collections.ts)
    /// と一对一 — camelCase付けると undefined 参照でUIが落ちる (WP-5の教訓)。
    #[test]
    fn dtos_serialize_snake_case_matching_frontend_types() {
        let dto = CollectionDto {
            id: "3".to_string(),
            uuid: "00000000-0000-7000-8000-000000000003".to_string(),
            name: "高評価".to_string(),
            kind: "smart".to_string(),
            query_json: Some(RATING_QUERY.to_string()),
            item_count: 12,
            created_at: "2026-08-31T00:00:00.000Z".to_string(),
            updated_at: "2026-08-31T00:00:00.000Z".to_string(),
        };
        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["item_count"], 12);
        assert_eq!(json["query_json"], RATING_QUERY);
        assert_eq!(json["created_at"], "2026-08-31T00:00:00.000Z");

        let items = CollectionItemsDto {
            total: 2,
            items: vec![CollectionItemDto {
                asset_id: "7".to_string(),
                position: 1,
                note: Some("メモ".to_string()),
            }],
        };
        let json = serde_json::to_value(&items).unwrap();
        assert_eq!(json["items"][0]["asset_id"], "7");
        assert_eq!(json["items"][0]["position"], 1);
        assert_eq!(json["items"][0]["note"], "メモ");

        let smart = SmartItemsDto {
            total: 1,
            asset_ids: vec!["9".to_string()],
        };
        let json = serde_json::to_value(&smart).unwrap();
        assert_eq!(json["asset_ids"][0], "9");

        let payload = CollectionsChangedPayload {
            reason: "items".to_string(),
            collection_id: Some("3".to_string()),
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["reason"], "items");
        assert_eq!(json["collection_id"], "3");
    }
}
