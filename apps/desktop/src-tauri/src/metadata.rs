//! アセットメタデータ編集コマンド (MVP WP-5)。
//!
//! 評価・選別・お気に入り・タグ・キャプションをstable AssetId
//! (assets.id。api-contract §19) 単位で永続化する。コマンドは薄い
//! ラッパーで、実体はテスト可能な `*_impl` 関数:
//!
//! - adhoc (未登録フォルダ一時走査) モードでは編集不可
//!   (仕様§4.1「未登録はライブラリ機能なし」)。カタログモードが
//!   Libraryでない場合、および仮ID (bit63。ハッシュ計算待ち) の場合に
//!   日本語エラーを返す。一時走査のIDは1開始の連番で assets.id と
//!   数値が偶発一致し得るため、モードガードが必須 (誤編集防止)。
//! - 書き込みは `Db::with_writer` (単一writer) 経由。一括適用はrepo側で
//!   1トランザクション (一部失敗時ロールバック)。
//! - 書き込み成功後、表示済みカタログレコードをin-place更新
//!   (generation不変) し `asset:updated` イベントをemitする。フロントは
//!   このイベントでセル/InfoPanel/セッション集計を局所更新する。
//! - 除外 (rejected) はファイルを削除しない (fs無操作。spec §9.2)。

use std::sync::Arc;

use gento_core::catalog::{CatalogMode, Triage};
use gento_core::library::PROVISIONAL_FILE_FLAG;
use gento_core::metadata::{AssetTagRepo, CaptionKind, CaptionRepo, TagRepo};
use gento_core::repo::{AssetRepo, RepoError, TriageState};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::catalog::{lock, AppState};

/// 変更種別 (`asset:updated` ペイロードの `changes` 要素)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldChange {
    Rating,
    Cull,
    Favorite,
    Tags,
    Caption,
}

impl FieldChange {
    const fn as_str(self) -> &'static str {
        match self {
            FieldChange::Rating => "rating",
            FieldChange::Cull => "cull",
            FieldChange::Favorite => "favorite",
            FieldChange::Tags => "tags",
            FieldChange::Caption => "caption",
        }
    }
}

/// `asset:updated` イベントペイロード。
/// `ids` は10進文字列化したstable AssetId (AssetIdのIPC規約)。
/// 変更した値も載せる (フロントはセル/InfoPanel/セッション集計を
/// イベント単体で局所更新できる。再取得不要)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AssetUpdatedPayload {
    pub ids: Vec<String>,
    pub changes: Vec<String>,
    /// 変更後の評価 (rating変更時のみ)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rating: Option<u8>,
    /// 変更後の選別状態 "unselected"|"picked"|"rejected" (cull変更時のみ)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub triage: Option<String>,
    /// 変更後のお気に入り (favorite変更時のみ)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub favorite: Option<bool>,
}

/// タグDTO (`asset_tags_list` / `tag_suggest` / `tag_list` 共通)。
/// snake_caseはWP-2委任仕様の表記規約 (ImageListItem系のcamelCaseとは別系統)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TagDto {
    /// 10進文字列。
    pub id: String,
    /// 表示名。
    pub name: String,
    /// 正規化名 (候補絞り込みの一致対象)。
    pub normalized_name: String,
    /// 付与されたAsset数。
    pub usage_count: u64,
    /// 手動編集済み (asset_tags_listのみ意味を持つ。一覧系はfalse)。
    pub manually_edited: bool,
}

/// キャプションDTO (`asset_detail` の `captions` 要素。現行のみ)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CaptionDto {
    /// "description" | "sd_tags" | "user_note"。
    pub kind: String,
    pub body: String,
    /// "manual" | "ai" | "import" | "embedded_metadata"。
    pub source: String,
    /// ユーザー編集済み (AI再生成保護の表示用。受入条件4)。
    pub manually_edited: bool,
    pub updated_at: String,
}

/// `asset_detail` の応答 (InfoPanelの一括取得用)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AssetDetailDto {
    /// 10進文字列 (要求したIDをそのまま返す)。
    pub id: String,
    pub rating: u8,
    /// "unselected" | "picked" | "rejected"。
    pub triage: String,
    pub favorite: bool,
    pub tags: Vec<TagDto>,
    /// 現行キャプション (未設定のkindは含めない)。
    pub captions: Vec<CaptionDto>,
}

/// DBを取り出す (未開時は日本語エラー)。
fn db_of(state: &AppState) -> Result<Arc<gento_db::Db>, String> {
    state
        .db
        .as_ref()
        .and_then(|d| d.db.clone())
        .ok_or_else(|| "データベースが利用できないため編集できません".to_string())
}

/// repoエラーをユーザー向け日本語へ変換する。
fn repo_error_message(err: RepoError) -> String {
    match err {
        RepoError::NotFound => {
            "対象の画像が見つかりません (削除された可能性があります)".to_string()
        }
        RepoError::Constraint(msg) => format!("値が不正です: {msg}"),
        other => format!("データベースエラー: {other}"),
    }
}

/// 編集対象のstable AssetId列を解決する。
///
/// - カタログがLibraryモードでなければ拒否 (adhocのIDはassets.idと
///   数値一致し得るため、モードで区別するしか安全な判別がない)
/// - 仮ID (bit63) は拒否 (ハッシュ計算待ち = Asset未確定)
pub(crate) fn parse_stable_asset_ids(
    mode: CatalogMode,
    ids: &[String],
) -> Result<Vec<i64>, String> {
    if mode != CatalogMode::Library {
        return Err(
            "未登録フォルダ表示中はメタデータを編集できません。登録ルートをライブラリとして表示してから編集してください"
                .to_string(),
        );
    }
    if ids.is_empty() {
        return Err("編集対象が空です".to_string());
    }
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let value: u64 = id.parse().map_err(|_| format!("無効なAssetIdです: {id}"))?;
        if value & PROVISIONAL_FILE_FLAG != 0 {
            return Err(
                "ハッシュ計算待ちの項目は編集できません (計算完了後に確定します)".to_string(),
            );
        }
        out.push(value as i64);
    }
    Ok(out)
}

/// 現在のカタログモードを読む。
fn current_mode(state: &AppState) -> CatalogMode {
    lock(&state.catalog).mode()
}

/// カタログレコードへメタデータ編集を反映し `asset:updated` をemitする。
/// `emit` はテストで記録用クロージャへ差し替え可能 (commandはAppHandle)。
/// `payload` の装飾 (rating等の変更後の値) は呼び出し側が追加する。
fn finish_field_update(
    state: &AppState,
    emit: &dyn Fn(&AssetUpdatedPayload),
    asset_ids: &[i64],
    patch: impl Fn(&mut gento_core::catalog::ImageRecord),
    change: FieldChange,
) {
    {
        let mut inner = lock(&state.catalog);
        inner.patch_library_assets(asset_ids, &patch);
    }
    emit(&AssetUpdatedPayload {
        ids: asset_ids.iter().map(|id| id.to_string()).collect(),
        changes: vec![change.as_str().to_string()],
        rating: None,
        triage: None,
        favorite: None,
    });
}

// ===== 実装本体 (テストから直接呼ぶ) =====

pub(crate) fn set_rating_impl(
    state: &AppState,
    emit: &dyn Fn(&AssetUpdatedPayload),
    ids: &[String],
    rating: u8,
) -> Result<(), String> {
    if rating > 5 {
        return Err("評価は0〜5で指定してください".to_string());
    }
    let asset_ids = parse_stable_asset_ids(current_mode(state), ids)?;
    let db = db_of(state)?;
    db.with_writer(|repos| repos.set_rating_batch(&asset_ids, rating))
        .map_err(repo_error_message)?;
    finish_field_update(
        state,
        &|payload| {
            emit(&AssetUpdatedPayload {
                rating: Some(rating),
                ..payload.clone()
            })
        },
        &asset_ids,
        |record| record.rating = rating,
        FieldChange::Rating,
    );
    Ok(())
}

pub(crate) fn set_cull_impl(
    state: &AppState,
    emit: &dyn Fn(&AssetUpdatedPayload),
    ids: &[String],
    cull: TriageState,
) -> Result<(), String> {
    let asset_ids = parse_stable_asset_ids(current_mode(state), ids)?;
    let db = db_of(state)?;
    db.with_writer(|repos| repos.set_cull_state_batch(&asset_ids, cull))
        .map_err(repo_error_message)?;
    let triage = match cull {
        TriageState::Unselected => Triage::Undecided,
        TriageState::Picked => Triage::Keep,
        TriageState::Rejected => Triage::Reject,
    };
    let cull_str = cull.as_str().to_string();
    finish_field_update(
        state,
        &|payload| {
            emit(&AssetUpdatedPayload {
                triage: Some(cull_str.clone()),
                ..payload.clone()
            })
        },
        &asset_ids,
        |record| record.triage = triage,
        FieldChange::Cull,
    );
    Ok(())
}

pub(crate) fn set_favorite_impl(
    state: &AppState,
    emit: &dyn Fn(&AssetUpdatedPayload),
    ids: &[String],
    favorite: bool,
) -> Result<(), String> {
    let asset_ids = parse_stable_asset_ids(current_mode(state), ids)?;
    let db = db_of(state)?;
    db.with_writer(|repos| repos.set_favorite_batch(&asset_ids, favorite))
        .map_err(repo_error_message)?;
    finish_field_update(
        state,
        &|payload| {
            emit(&AssetUpdatedPayload {
                favorite: Some(favorite),
                ..payload.clone()
            })
        },
        &asset_ids,
        |record| record.favorite = favorite,
        FieldChange::Favorite,
    );
    Ok(())
}

pub(crate) fn tags_add_impl(
    state: &AppState,
    emit: &dyn Fn(&AssetUpdatedPayload),
    ids: &[String],
    names: Vec<String>,
) -> Result<(), String> {
    if names.is_empty() {
        return Err("タグ名が空です".to_string());
    }
    let name_refs: Vec<&str> = names.iter().map(|n| n.as_str()).collect();
    let asset_ids = parse_stable_asset_ids(current_mode(state), ids)?;
    let db = db_of(state)?;
    db.with_writer(|repos| repos.add_tags_batch(&asset_ids, &name_refs))
        .map_err(repo_error_message)?;
    finish_field_update(state, emit, &asset_ids, |_| {}, FieldChange::Tags);
    Ok(())
}

pub(crate) fn tags_remove_impl(
    state: &AppState,
    emit: &dyn Fn(&AssetUpdatedPayload),
    asset_id: String,
    tag_id: String,
) -> Result<(), String> {
    let ids = parse_stable_asset_ids(current_mode(state), &[asset_id])?;
    let tag_row: i64 = tag_id
        .parse()
        .map_err(|_| format!("無効なタグIDです: {tag_id}"))?;
    let db = db_of(state)?;
    db.with_writer(|repos| repos.remove_tag_from_asset(ids[0], tag_row))
        .map_err(repo_error_message)?;
    finish_field_update(state, emit, &ids, |_| {}, FieldChange::Tags);
    Ok(())
}

fn tag_dto(tag: &gento_core::metadata::TagRecord, manually_edited: bool) -> TagDto {
    TagDto {
        id: tag.id.to_string(),
        name: tag.display_name.clone(),
        normalized_name: tag.normalized_name.clone(),
        usage_count: tag.usage_count,
        manually_edited,
    }
}

pub(crate) fn tags_list_impl(state: &AppState, asset_id: String) -> Result<Vec<TagDto>, String> {
    let ids = parse_stable_asset_ids(current_mode(state), &[asset_id])?;
    let db = db_of(state)?;
    db.with_reader(|repos| repos.list_asset_tags(ids[0]))
        .map(|records| {
            records
                .iter()
                .map(|r| tag_dto(&r.tag, r.manually_edited))
                .collect()
        })
        .map_err(repo_error_message)
}

pub(crate) fn tag_suggest_impl(
    state: &AppState,
    prefix: String,
    limit: Option<u32>,
) -> Result<Vec<TagDto>, String> {
    let limit = limit.unwrap_or(20).clamp(1, 100);
    let db = db_of(state)?;
    db.with_reader(|repos| repos.suggest_tags(&prefix, limit))
        .map(|records| records.iter().map(|t| tag_dto(t, false)).collect())
        .map_err(repo_error_message)
}

pub(crate) fn tag_list_impl(state: &AppState) -> Result<Vec<TagDto>, String> {
    let db = db_of(state)?;
    db.with_reader(|repos| repos.list_tags())
        .map(|records| records.iter().map(|t| tag_dto(t, false)).collect())
        .map_err(repo_error_message)
}

pub(crate) fn tag_delete_impl(
    state: &AppState,
    emit: &dyn Fn(&AssetUpdatedPayload),
    tag_id: String,
) -> Result<(), String> {
    let tag_row: i64 = tag_id
        .parse()
        .map_err(|_| format!("無効なタグIDです: {tag_id}"))?;
    let db = db_of(state)?;
    // 削除前に付与されているAssetを特定 (イベント送出対象)
    let affected: Vec<i64> = db
        .with_reader(|repos| repos.assets_of_tag(tag_row))
        .map_err(repo_error_message)?;
    db.with_writer(|repos| repos.delete_tag(tag_row))
        .map_err(repo_error_message)?;
    if !affected.is_empty() {
        finish_field_update(state, emit, &affected, |_| {}, FieldChange::Tags);
    }
    Ok(())
}

fn caption_kind_of(value: &str) -> Result<CaptionKind, String> {
    match value {
        "description" => Ok(CaptionKind::Description),
        "sd_tags" => Ok(CaptionKind::SdTags),
        "user_note" => Ok(CaptionKind::UserNote),
        other => Err(format!(
            "不明なキャプション種別です: {other} (description|sd_tags|user_note)"
        )),
    }
}

pub(crate) fn caption_get_impl(
    state: &AppState,
    asset_id: String,
    kind: String,
) -> Result<Option<CaptionDto>, String> {
    let ids = parse_stable_asset_ids(current_mode(state), &[asset_id])?;
    let kind = caption_kind_of(&kind)?;
    let db = db_of(state)?;
    db.with_reader(|repos| repos.get_current_caption(ids[0], kind))
        .map(|record| {
            record.map(|c| CaptionDto {
                kind: c.kind.as_str().to_string(),
                body: c.body,
                source: c.source.as_str().to_string(),
                manually_edited: c.manually_edited,
                updated_at: c.updated_at,
            })
        })
        .map_err(repo_error_message)
}

pub(crate) fn caption_set_impl(
    state: &AppState,
    emit: &dyn Fn(&AssetUpdatedPayload),
    asset_id: String,
    kind: String,
    body: String,
) -> Result<CaptionDto, String> {
    let ids = parse_stable_asset_ids(current_mode(state), std::slice::from_ref(&asset_id))?;
    let kind = caption_kind_of(&kind)?;
    let db = db_of(state)?;
    let record = db
        .with_writer(|repos| repos.set_caption(ids[0], kind, &body))
        .map_err(repo_error_message)?;
    finish_field_update(state, emit, &ids, |_| {}, FieldChange::Caption);
    Ok(CaptionDto {
        kind: record.kind.as_str().to_string(),
        body: record.body,
        source: record.source.as_str().to_string(),
        manually_edited: record.manually_edited,
        updated_at: record.updated_at,
    })
}

pub(crate) fn asset_detail_impl(
    state: &AppState,
    asset_id: String,
) -> Result<AssetDetailDto, String> {
    let ids = parse_stable_asset_ids(current_mode(state), std::slice::from_ref(&asset_id))?;
    let db = db_of(state)?;
    db.with_reader(|repos| {
        let asset = repos.get_asset(ids[0])?.ok_or(RepoError::NotFound)?;
        let tags = repos.list_asset_tags(ids[0])?;
        let mut captions = Vec::new();
        for kind in CaptionKind::EDITABLE {
            if let Some(c) = repos.get_current_caption(ids[0], kind)? {
                captions.push(CaptionDto {
                    kind: c.kind.as_str().to_string(),
                    body: c.body,
                    source: c.source.as_str().to_string(),
                    manually_edited: c.manually_edited,
                    updated_at: c.updated_at,
                });
            }
        }
        Ok(AssetDetailDto {
            id: asset_id.clone(),
            rating: asset.rating,
            triage: asset.triage.as_str().to_string(),
            favorite: asset.favorite,
            tags: tags
                .iter()
                .map(|r| tag_dto(&r.tag, r.manually_edited))
                .collect(),
            captions,
        })
    })
    .map_err(repo_error_message)
}

// ===== Tauri command (薄ラッパ) =====

/// command用のemitter (`asset:updated` をAppHandle経由でemit)。
fn tauri_emitter<'a>(app: &'a AppHandle) -> impl Fn(&AssetUpdatedPayload) + use<'a> {
    move |payload| {
        let _ = app.emit("asset:updated", payload);
    }
}

#[tauri::command]
pub fn asset_set_rating(
    app: AppHandle,
    state: State<'_, AppState>,
    ids: Vec<String>,
    rating: u8,
) -> Result<(), String> {
    set_rating_impl(&state, &tauri_emitter(&app), &ids, rating)
}

#[tauri::command]
pub fn asset_set_cull(
    app: AppHandle,
    state: State<'_, AppState>,
    ids: Vec<String>,
    cull: String,
) -> Result<(), String> {
    let parsed = match cull.as_str() {
        "picked" => TriageState::Picked,
        "rejected" => TriageState::Rejected,
        "unselected" => TriageState::Unselected,
        other => {
            return Err(format!(
                "不明な選別状態です: {other} (picked|rejected|unselected)"
            ))
        }
    };
    set_cull_impl(&state, &tauri_emitter(&app), &ids, parsed)
}

#[tauri::command]
pub fn asset_set_favorite(
    app: AppHandle,
    state: State<'_, AppState>,
    ids: Vec<String>,
    favorite: bool,
) -> Result<(), String> {
    set_favorite_impl(&state, &tauri_emitter(&app), &ids, favorite)
}

#[tauri::command]
pub fn asset_tags_add(
    app: AppHandle,
    state: State<'_, AppState>,
    ids: Vec<String>,
    names: Vec<String>,
) -> Result<(), String> {
    tags_add_impl(&state, &tauri_emitter(&app), &ids, names)
}

#[tauri::command]
pub fn asset_tags_remove(
    app: AppHandle,
    state: State<'_, AppState>,
    asset_id: String,
    tag_id: String,
) -> Result<(), String> {
    tags_remove_impl(&state, &tauri_emitter(&app), asset_id, tag_id)
}

#[tauri::command]
pub fn asset_tags_list(
    state: State<'_, AppState>,
    asset_id: String,
) -> Result<Vec<TagDto>, String> {
    tags_list_impl(&state, asset_id)
}

#[tauri::command]
pub fn tag_suggest(
    state: State<'_, AppState>,
    prefix: String,
    limit: Option<u32>,
) -> Result<Vec<TagDto>, String> {
    tag_suggest_impl(&state, prefix, limit)
}

#[tauri::command]
pub fn tag_list(state: State<'_, AppState>) -> Result<Vec<TagDto>, String> {
    tag_list_impl(&state)
}

#[tauri::command]
pub fn tag_delete(
    app: AppHandle,
    state: State<'_, AppState>,
    tag_id: String,
) -> Result<(), String> {
    tag_delete_impl(&state, &tauri_emitter(&app), tag_id)
}

#[tauri::command]
pub fn asset_caption_get(
    state: State<'_, AppState>,
    asset_id: String,
    kind: String,
) -> Result<Option<CaptionDto>, String> {
    caption_get_impl(&state, asset_id, kind)
}

#[tauri::command]
pub fn asset_caption_set(
    app: AppHandle,
    state: State<'_, AppState>,
    asset_id: String,
    kind: String,
    body: String,
) -> Result<CaptionDto, String> {
    caption_set_impl(&state, &tauri_emitter(&app), asset_id, kind, body)
}

#[tauri::command]
pub fn asset_detail(
    state: State<'_, AppState>,
    asset_id: String,
) -> Result<AssetDetailDto, String> {
    asset_detail_impl(&state, asset_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::AppState;
    use gento_core::catalog::{CatalogMode, ImageRecord, ImageState, ThumbnailState};
    use gento_core::AssetId;
    use std::path::PathBuf;
    use std::sync::Mutex as StdMutex;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "gento-tauri-meta-{tag}-{}-{}",
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

    /// Libraryモード + テストDB + 1Asset (assets.id=1) を持つAppState。
    struct TestApp {
        _tmp: TempDir,
        state: AppState,
        events: StdMutex<Vec<AssetUpdatedPayload>>,
    }

    impl TestApp {
        fn new(tag: &str) -> Self {
            let tmp = TempDir::new(tag);
            let db = gento_db::Db::open(tmp.0.join("db.sqlite3")).unwrap();
            let mut state = AppState::new(PathBuf::from("/tmp/gento-meta-dataset"));
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

        fn seed_asset(&self, hash: &str) -> i64 {
            self.db()
                .with_writer(|repos| {
                    repos
                        .create_asset(gento_core::repo::NewAsset {
                            content_hash: format!("blake3:{hash}"),
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
                .unwrap()
        }

        /// Libraryモード表示レコードを投入 (asset_row_idをstable IDへ)。
        fn seed_library_record(&self, asset_row_id: i64) {
            let mut inner = lock(&self.state.catalog);
            inner.push_library_record_for_test(ImageRecord {
                id: AssetId::new(asset_row_id as u64),
                source_path: PathBuf::from("/data/x.png"),
                display_name: "x.png".to_string(),
                width: None,
                height: None,
                byte_size: 1,
                mtime_ms: 1,
                mime_type: None,
                is_animated: false,
                state: ImageState::Ready,
                thumb_state: ThumbnailState::Missing,
                rating: 0,
                triage: Triage::Undecided,
                favorite: false,
            });
        }

        fn emit(&self, payload: &AssetUpdatedPayload) {
            self.events.lock().unwrap().push(payload.clone());
        }
    }

    fn with_library_app<T>(tag: &str, f: impl FnOnce(&TestApp) -> T) -> T {
        let app = TestApp::new(tag);
        f(&app)
    }

    #[test]
    fn adhoc_mode_rejects_edits() {
        let app = TestApp::new("adhoc");
        // Real (一時走査) モードへ戻す
        {
            let mut inner = lock(&app.state.catalog);
            inner.force_mode_for_test(CatalogMode::Real);
        }
        let err = set_rating_impl(&app.state, &|p| app.emit(p), &["1".to_string()], 3).unwrap_err();
        assert!(err.contains("未登録フォルダ"), "{err}");
        // Virtual (devtools) も同様
        {
            let mut inner = lock(&app.state.catalog);
            inner.force_mode_for_test(CatalogMode::Virtual);
        }
        let err = tags_add_impl(
            &app.state,
            &|p| app.emit(p),
            &["1".to_string()],
            vec!["tag".to_string()],
        )
        .unwrap_err();
        assert!(err.contains("未登録フォルダ"), "{err}");
    }

    #[test]
    fn provisional_id_rejects_edits() {
        with_library_app("provisional", |app| {
            // bit63仮ID (ハッシュ待ち)
            let provisional = format!("{}", (1u64 << 63) | 5);
            let err = set_rating_impl(&app.state, &|p| app.emit(p), &[provisional], 3).unwrap_err();
            assert!(err.contains("ハッシュ計算待ち"), "{err}");
        });
    }

    #[test]
    fn invalid_inputs_are_rejected_in_japanese() {
        with_library_app("invalid", |app| {
            let err = set_rating_impl(&app.state, &|p| app.emit(p), &[], 3).unwrap_err();
            assert!(err.contains("編集対象が空"), "{err}");
            let err =
                set_rating_impl(&app.state, &|p| app.emit(p), &["abc".to_string()], 3).unwrap_err();
            assert!(err.contains("無効なAssetId"), "{err}");
            let err =
                set_rating_impl(&app.state, &|p| app.emit(p), &["1".to_string()], 6).unwrap_err();
            assert!(err.contains("0〜5"), "{err}");
            let err = caption_get_impl(&app.state, "1".to_string(), "unknown_kind".to_string())
                .unwrap_err();
            assert!(err.contains("不明なキャプション種別"), "{err}");
        });
    }

    #[test]
    fn rating_cull_favorite_apply_and_emit() {
        with_library_app("fields", |app| {
            let a1 = app.seed_asset("f1");
            let a2 = app.seed_asset("f2");
            app.seed_library_record(a1);
            app.seed_library_record(a2);
            let ids = [a1.to_string(), a2.to_string()];

            set_rating_impl(&app.state, &|p| app.emit(p), &ids, 4).unwrap();
            set_cull_impl(&app.state, &|p| app.emit(p), &ids, TriageState::Rejected).unwrap();
            set_favorite_impl(&app.state, &|p| app.emit(p), &ids, true).unwrap();

            // DB永続化
            for id in [a1, a2] {
                let asset = app.db().with_reader(|r| r.get_asset(id)).unwrap().unwrap();
                assert_eq!(
                    (asset.rating, asset.triage, asset.favorite),
                    (4, TriageState::Rejected, true)
                );
            }
            // カタログレコードのin-place更新
            {
                let inner = lock(&app.state.catalog);
                for record in inner.records() {
                    assert_eq!(record.rating, 4);
                    assert_eq!(record.triage, Triage::Reject);
                    assert!(record.favorite);
                }
            }
            // イベント発火 (rating/cull/favoriteの3回・idsは両方)
            {
                let events = app.events.lock().unwrap();
                assert_eq!(events.len(), 3);
                assert_eq!(events[0].changes, vec!["rating".to_string()]);
                assert_eq!(events[1].changes, vec!["cull".to_string()]);
                assert_eq!(events[2].changes, vec!["favorite".to_string()]);
                // 変更後の値もペイロードに載る (フロントの局所更新用)
                assert_eq!(events[0].rating, Some(4));
                assert_eq!(events[1].triage.as_deref(), Some("rejected"));
                assert_eq!(events[2].favorite, Some(true));
                for event in events.iter() {
                    assert_eq!(event.ids, ids);
                }
            }

            // cull文字列パースはコマンド層の責務だが不変値もここで担保
            let err = set_cull_impl(&app.state, &|p| app.emit(p), &ids, TriageState::Unselected);
            assert!(err.is_ok());
        });
    }

    #[test]
    fn batch_with_missing_asset_rolls_back_and_errors() {
        with_library_app("batchmiss", |app| {
            let a1 = app.seed_asset("b1");
            app.seed_library_record(a1);
            let err = set_rating_impl(
                &app.state,
                &|p| app.emit(p),
                &[a1.to_string(), "9999".to_string()],
                5,
            )
            .unwrap_err();
            assert!(err.contains("見つかりません"), "{err}");
            let asset = app.db().with_reader(|r| r.get_asset(a1)).unwrap().unwrap();
            assert_eq!(asset.rating, 0, "ロールバック済み");
            assert!(
                app.events.lock().unwrap().is_empty(),
                "失敗時はイベント無し"
            );
        });
    }

    #[test]
    fn tags_flow_add_list_suggest_remove_delete() {
        with_library_app("tags", |app| {
            let a1 = app.seed_asset("tg1");
            let a2 = app.seed_asset("tg2");
            app.seed_library_record(a1);
            app.seed_library_record(a2);

            // 追加 (正規化: 大文字/空白)
            tags_add_impl(
                &app.state,
                &|p| app.emit(p),
                &[a1.to_string(), a2.to_string()],
                vec!["  Sky ".to_string(), "blue".to_string()],
            )
            .unwrap();

            let tags = tags_list_impl(&app.state, a1.to_string()).unwrap();
            assert_eq!(tags.len(), 2);
            assert_eq!(tags[0].name, "blue");
            assert!(tags
                .iter()
                .any(|t| t.normalized_name == "sky" && t.manually_edited));

            // 候補
            let hits = tag_suggest_impl(&app.state, "SKY".to_string(), None).unwrap();
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].usage_count, 2);
            let all = tag_list_impl(&app.state).unwrap();
            assert_eq!(all.len(), 2);

            // 解除
            let sky_id = tags
                .iter()
                .find(|t| t.normalized_name == "sky")
                .unwrap()
                .id
                .clone();
            tags_remove_impl(&app.state, &|p| app.emit(p), a1.to_string(), sky_id.clone()).unwrap();
            assert_eq!(tags_list_impl(&app.state, a1.to_string()).unwrap().len(), 1);

            // タグ削除 (付随解除。a2からも消える)
            tag_delete_impl(&app.state, &|p| app.emit(p), sky_id.clone()).unwrap();
            assert_eq!(
                tags_list_impl(&app.state, a2.to_string()).unwrap().len(),
                1,
                "blueのみ残る"
            );
            let err = tag_delete_impl(&app.state, &|p| app.emit(p), sky_id).unwrap_err();
            assert!(err.contains("見つかりません"), "{err}");
        });
    }

    #[test]
    fn caption_set_get_and_detail() {
        with_library_app("caption", |app| {
            let a1 = app.seed_asset("cp1");
            app.seed_library_record(a1);

            let initial =
                caption_get_impl(&app.state, a1.to_string(), "description".to_string()).unwrap();
            assert!(initial.is_none());

            let set = caption_set_impl(
                &app.state,
                &|p| app.emit(p),
                a1.to_string(),
                "description".to_string(),
                "風景の写真".to_string(),
            )
            .unwrap();
            assert_eq!(set.body, "風景の写真");
            assert!(set.manually_edited);
            assert_eq!(set.source, "manual");

            caption_set_impl(
                &app.state,
                &|p| app.emit(p),
                a1.to_string(),
                "sd_tags".to_string(),
                "sky, mountain".to_string(),
            )
            .unwrap();

            let detail = asset_detail_impl(&app.state, a1.to_string()).unwrap();
            assert_eq!(detail.rating, 0);
            assert_eq!(detail.triage, "unselected");
            assert!(!detail.favorite);
            assert!(detail.tags.is_empty());
            assert_eq!(detail.captions.len(), 2, "未設定のuser_noteは含まれない");
            assert!(detail
                .captions
                .iter()
                .any(|c| c.kind == "sd_tags" && c.body == "sky, mountain"));
            assert!(detail.captions.iter().all(|c| c.manually_edited));
        });
    }

    #[test]
    fn detail_missing_asset_is_not_found() {
        with_library_app("missing", |app| {
            let err = asset_detail_impl(&app.state, "9999".to_string()).unwrap_err();
            assert!(err.contains("見つかりません"), "{err}");
        });
    }

    #[test]
    fn parse_stable_asset_ids_mode_and_value_matrix() {
        use super::parse_stable_asset_ids;
        assert!(parse_stable_asset_ids(CatalogMode::Library, &["1".to_string()]).is_ok());
        assert!(
            parse_stable_asset_ids(CatalogMode::Library, &["42".to_string(), "7".to_string()])
                .is_ok()
        );
        assert!(parse_stable_asset_ids(CatalogMode::Real, &["1".to_string()]).is_err());
        assert!(parse_stable_asset_ids(CatalogMode::Virtual, &["1".to_string()]).is_err());
        // 仮ID拒否
        let provisional = ((1u64 << 63) | 3).to_string();
        assert!(parse_stable_asset_ids(CatalogMode::Library, &[provisional]).is_err());
        // u64範囲外
        assert!(parse_stable_asset_ids(
            CatalogMode::Library,
            &["99999999999999999999999".to_string()]
        )
        .is_err());
    }

    /// WP-5 DTOの直列化形状 (snake_case)。フロントの型定義
    /// (lib/metadata.ts) と一对一であることを固定する — camelCaseにすると
    /// `tag.usage_count` がundefinedになりUI描画が落ちる (実機E2Eで発見)。
    #[test]
    fn dtos_serialize_snake_case_matching_frontend_types() {
        let dto = AssetDetailDto {
            id: "1".to_string(),
            rating: 3,
            triage: "picked".to_string(),
            favorite: true,
            tags: vec![TagDto {
                id: "7".to_string(),
                name: "Sky".to_string(),
                normalized_name: "sky".to_string(),
                usage_count: 2,
                manually_edited: true,
            }],
            captions: vec![CaptionDto {
                kind: "user_note".to_string(),
                body: "メモ".to_string(),
                source: "manual".to_string(),
                manually_edited: true,
                updated_at: "2026-08-31T00:00:00.000Z".to_string(),
            }],
        };
        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["tags"][0]["normalized_name"], "sky");
        assert_eq!(json["tags"][0]["usage_count"], 2);
        assert_eq!(json["tags"][0]["manually_edited"], true);
        assert_eq!(json["captions"][0]["manually_edited"], true);
        assert_eq!(
            json["captions"][0]["updated_at"],
            "2026-08-31T00:00:00.000Z"
        );
        assert_eq!(json["triage"], "picked");
        assert_eq!(json["rating"], 3);
    }
}
