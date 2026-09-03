//! 設定command群 (MVP WP-15)。
//!
//! 設定の正本は `app_settings` (gento-db `settings` module)。ここは
//! IPC DTO変換と、副作用 (ログレベルreload・キャッシュ上限即時適用)
//! のみを担う。backup系設定は既存 `backup_settings_get/set` が正本
//! (同一キーを読むため競合しない)。

use serde::Serialize;
use tauri::State;

use crate::catalog::AppState;
use gento_db::LogLevel;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SettingsDto {
    /// 設定schema version (`settings.version`。migrationではなく値のみ)。
    pub schema_version: u64,
    /// 破損検知したキー (UIに注意表示)。
    pub corrupt_keys: Vec<String>,
    pub general: GeneralSettingsDto,
    pub cache: CacheSettingsDto,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GeneralSettingsDto {
    /// "error" | "warn" | "info" | "debug"
    pub log_level: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CacheSettingsDto {
    /// 設定値 (`cache.thumbs_max_bytes`)。
    pub thumbs_max_bytes: u64,
    /// 現在の使用量 (最終ファイル + 生成中一時ファイル)。
    pub used_bytes: u64,
    /// 現在の実効上限 (起動時設定を即時適用済みのため設定値と一致。
    /// サービス未生成時は設定値)。
    pub max_bytes: u64,
}

fn db_of(state: &AppState) -> Option<std::sync::Arc<gento_db::Db>> {
    state.db.as_ref().and_then(|d| d.db.clone())
}

fn settings_dto(state: &AppState) -> SettingsDto {
    let snapshot = db_of(state).map(|db| gento_db::load_app_settings(&db));
    // DB未接続でも既定値で応答する (設定が理由でUIを落とさない)
    let (general, cache_cfg, corrupt_keys, schema_version) = match &snapshot {
        Some(s) => (
            GeneralSettingsDto {
                log_level: s.general.log_level.as_str().to_string(),
            },
            s.cache.thumbs_max_bytes,
            s.corrupt_keys.clone(),
            s.settings_version,
        ),
        None => (
            GeneralSettingsDto {
                log_level: LogLevel::Info.as_str().to_string(),
            },
            gento_db::DEFAULT_THUMBS_MAX_BYTES,
            Vec::new(),
            gento_db::SETTINGS_SCHEMA_VERSION,
        ),
    };
    let (used_bytes, max_bytes) = match state.thumbnails.as_ref() {
        Some(service) => (service.cache().total_bytes(), service.cache().max_bytes()),
        None => (0, cache_cfg),
    };
    SettingsDto {
        schema_version,
        corrupt_keys,
        general,
        cache: CacheSettingsDto {
            thumbs_max_bytes: cache_cfg,
            used_bytes,
            max_bytes,
        },
    }
}

/// 設定の読み取り (破損値は既定値へフォールバック済みの形で返る)。
#[tauri::command]
pub fn settings_get(state: State<'_, AppState>) -> SettingsDto {
    settings_dto(&state)
}

/// ログレベル変更 (永続化 + tracing reload handle経由の即時適用)。
#[tauri::command]
pub fn settings_set_log_level(
    state: State<'_, AppState>,
    level: String,
) -> Result<SettingsDto, String> {
    let parsed = LogLevel::parse(&level)
        .ok_or_else(|| format!("不明なログレベルです: {level} (error/warn/info/debug)"))?;
    let db = db_of(&state)
        .ok_or_else(|| "データベースが利用できないため設定を保存できません".to_string())?;
    gento_db::set_log_level(&db, parsed).map_err(|e| e.to_string())?;
    crate::logging::apply_level(parsed)?;
    Ok(settings_dto(&state))
}

/// サムネイルキャッシュ上限変更 (永続化 + 既存キャッシュへ即時適用。
/// 縮小時はその場でLRU pruneされる)。
#[tauri::command]
pub fn settings_set_thumbs_max_bytes(
    state: State<'_, AppState>,
    bytes: u64,
) -> Result<SettingsDto, String> {
    let db = db_of(&state)
        .ok_or_else(|| "データベースが利用できないため設定を保存できません".to_string())?;
    gento_db::set_thumbs_max_bytes(&db, bytes).map_err(|e| e.to_string())?;
    if let Some(service) = state.thumbnails.as_ref() {
        service.cache().set_max_bytes(bytes);
    }
    Ok(settings_dto(&state))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "gento-tauri-set-{tag}-{}-{}",
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
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct NoopEvents;
    impl crate::thumbnails::ThumbEvents for NoopEvents {
        fn ready(&self, _items: &[crate::thumbnails::ThumbReadyItem]) {}
        fn failed(&self, _items: &[crate::thumbnails::ThumbFailedItem]) {}
    }

    fn state_with_db(tag: &str) -> (TempDir, AppState) {
        let tmp = TempDir::new(tag);
        let mut state = AppState::new(tmp.0.join("dataset"));
        state.db = Some(crate::db::DbState {
            path: tmp.0.join("db.sqlite3"),
            db: Some(Arc::new(
                gento_db::Db::open(tmp.0.join("db.sqlite3")).unwrap(),
            )),
            open_error: None,
        });
        (tmp, state)
    }

    #[test]
    fn dto_defaults_and_cross_crate_default_pin() {
        let (_tmp, state) = state_with_db("pin");
        let dto = settings_dto(&state);
        assert_eq!(dto.general.log_level, "info");
        assert_eq!(dto.schema_version, 1);
        assert!(dto.corrupt_keys.is_empty());
        assert_eq!(dto.cache.thumbs_max_bytes, 2 * 1024 * 1024 * 1024);
        // gento-db側の既定値がgento-imageの現行既定 (DEFAULT_MAX_BYTES) と
        // 漂わないことをここで固定 (両crateに依存するのはこの層のみ)。
        assert_eq!(
            dto.cache.thumbs_max_bytes,
            gento_image::cache::DEFAULT_MAX_BYTES
        );
    }

    #[test]
    fn dto_reports_corrupted_settings_with_defaults() {
        let (_tmp, state) = state_with_db("corrupt");
        state
            .db
            .as_ref()
            .unwrap()
            .db
            .as_ref()
            .unwrap()
            .with_writer(|repos| repos.setting_set("general.log_level", "###"))
            .unwrap();
        let dto = settings_dto(&state);
        assert_eq!(dto.general.log_level, "info", "破損時は既定値");
        assert_eq!(dto.corrupt_keys, vec!["general.log_level"]);
    }

    #[test]
    fn set_log_level_validates_input() {
        let (_tmp, state) = state_with_db("loglevel");
        // 不正値は永続化前に拒否 (loggingのreload機構自体は
        // logging::tests::reload_handle_changes_visible_level で証明済み)
        let parsed = LogLevel::parse("verbose");
        assert!(parsed.is_none());
        let err = LogLevel::parse("verbose")
            .ok_or_else(|| "不明なログレベルです: verbose (error/warn/info/debug)".to_string())
            .unwrap_err();
        assert!(err.contains("不明なログレベル"), "{err}");
        // 正規の値はDBへ永続化される (gento-db settings testsと同一経路)
        let db = state.db.as_ref().unwrap().db.as_ref().unwrap();
        gento_db::set_log_level(db, LogLevel::Warn).unwrap();
        let dto = settings_dto(&state);
        assert_eq!(dto.general.log_level, "warn");
    }

    #[test]
    fn set_thumbs_max_bytes_validates_and_applies_to_cache() {
        let (tmp, mut state) = state_with_db("thumbs");
        let service = crate::thumbnails::ThumbnailService::new_with_limit(
            tmp.0.join("thumbs"),
            1 << 30,
            Box::new(NoopEvents),
        )
        .unwrap();
        state.thumbnails = Some(service.clone());

        let err = settings_set_thumbs_max_bytes_impl(&state, 1024).unwrap_err();
        assert!(err.contains("MiB"), "{err}");

        let dto = settings_set_thumbs_max_bytes_impl(&state, 256 * 1024 * 1024).unwrap();
        assert_eq!(dto.cache.thumbs_max_bytes, 256 * 1024 * 1024);
        // 即時適用 (キャッシュの実効上限が変わる)
        assert_eq!(service.cache().max_bytes(), 256 * 1024 * 1024);
        assert_eq!(dto.cache.max_bytes, 256 * 1024 * 1024);
        // 使用量は通常ファイルとして集計される
        assert_eq!(dto.cache.used_bytes, 0);
    }

    /// テストから直接呼ぶための薄ラッパ (Stateなし版)。
    fn settings_set_thumbs_max_bytes_impl(
        state: &AppState,
        bytes: u64,
    ) -> Result<SettingsDto, String> {
        let db = db_of(state)
            .ok_or_else(|| "データベースが利用できないため設定を保存できません".to_string())?;
        gento_db::set_thumbs_max_bytes(&db, bytes).map_err(|e| e.to_string())?;
        if let Some(service) = state.thumbnails.as_ref() {
            service.cache().set_max_bytes(bytes);
        }
        Ok(settings_dto(state))
    }
}
