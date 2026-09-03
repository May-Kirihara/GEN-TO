//! アプリ設定の型付きaccessor (MVP WP-15)。
//!
//! # 設計契約 (PM決定)
//!
//! - 設定は `app_settings` キー値テーブルに**名前空間付きキー**で保存する
//!   (`general.*` / `cache.*` / `search.*` / `backup.*` 既存)。**DB migrationは
//!   行わない** — schema version相当は `settings.version` 値のみ。
//! - 値は原則JSON直列化 (`"warn"` / `2147483648`)。既存のbackup系キー
//!   (`backup.auto_full_db` = `off` 等の素の文字列) は**互換のため素の形式も
//!   受け入れる** (既存コマンド `backup_settings_get/set` が読み書きするため)。
//! - **破損値・範囲外・未知値 → 既定値 + warnログで継続** (受け入れ条件
//!   「設定破損時に安全な既定値で起動」)。破損検知数は診断スナップショット
//!   (`settings_corrupt_count`) へ出す。
//! - 既定値は「現状挙動と同値」: サムネイル上限は gento-image
//!   `cache::DEFAULT_MAX_BYTES` (2 GiB) と同値。gento-dbはgento-imageに
//!   依存しないため値をここに重複定義し、テストで同値を固定する
//!   (gento-image側変更時は両方更新すること)。

use gento_core::repo::RepoError;

use crate::backup::{
    AutoBackupFrequency, DEFAULT_BACKUP_RETENTION, SETTING_BACKUP_AUTO_FULL_DB,
    SETTING_BACKUP_RETENTION,
};
use crate::ids::now_utc_rfc3339;
use crate::Db;

/// app_settings: ログレベル (`error|warn|info|debug`。JSON文字列)。
pub const SETTING_GENERAL_LOG_LEVEL: &str = "general.log_level";
/// app_settings: サムネイルキャッシュ上限バイト数 (JSON数値)。
pub const SETTING_CACHE_THUMBS_MAX_BYTES: &str = "cache.thumbs_max_bytes";
/// app_settings: FTS索引の最終全再構築時刻 (RFC3339。WP-15診断用)。
pub const SETTING_FTS_LAST_REBUILD: &str = "fts.last_rebuild_at";
/// app_settings: 設定schema version (PM決定「migration不要・version値のみ」)。
pub const SETTING_SETTINGS_VERSION: &str = "settings.version";

/// 設定schemaの現行version。
pub const SETTINGS_SCHEMA_VERSION: u64 = 1;

/// サムネイルキャッシュ上限の既定値 (= gento-image `cache::DEFAULT_MAX_BYTES`
/// と同値。上のmodule doc参照)。
pub const DEFAULT_THUMBS_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// `cache.thumbs_max_bytes` の受け入れ範囲 (下限 = 極端なprune連発防止)。
pub const MIN_THUMBS_MAX_BYTES: u64 = 64 * 1024 * 1024;
/// `cache.thumbs_max_bytes` の受け入れ範囲上限。
pub const MAX_THUMBS_MAX_BYTES: u64 = 1024 * 1024 * 1024 * 1024;

/// ログレベル (tracing_subscriberのEnvFilter全体指定に使う4値)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
}

impl LogLevel {
    pub const fn as_str(self) -> &'static str {
        match self {
            LogLevel::Error => "error",
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
        }
    }

    /// 設定値の解析 (大文字小文字不問)。不正値はNone。
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "error" => Some(LogLevel::Error),
            "warn" => Some(LogLevel::Warn),
            "info" => Some(LogLevel::Info),
            "debug" => Some(LogLevel::Debug),
            _ => None,
        }
    }
}

/// 読み取り済みアプリ設定スナップショット (破損は既定値へフォールバック)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppSettingsSnapshot {
    pub general: GeneralSettings,
    pub cache: CacheSettings,
    pub backup: BackupSettingsView,
    /// FTS索引の最終全再構築時刻 (未実施 = None)。
    pub fts_last_rebuild_at: Option<String>,
    /// `settings.version` 行の値 (未設定 = 現行version)。
    pub settings_version: u64,
    /// 解析に失敗して既定値へフォールバックしたキー (診断表示用)。
    pub corrupt_keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneralSettings {
    pub log_level: LogLevel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheSettings {
    pub thumbs_max_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupSettingsView {
    pub auto_full_db: AutoBackupFrequency,
    pub retention_generations: u32,
}

impl Default for GeneralSettings {
    fn default() -> Self {
        GeneralSettings {
            log_level: LogLevel::Info,
        }
    }
}

impl Default for CacheSettings {
    fn default() -> Self {
        CacheSettings {
            thumbs_max_bytes: DEFAULT_THUMBS_MAX_BYTES,
        }
    }
}

impl Default for BackupSettingsView {
    fn default() -> Self {
        BackupSettingsView {
            auto_full_db: AutoBackupFrequency::Off,
            retention_generations: DEFAULT_BACKUP_RETENTION as u32,
        }
    }
}

impl AppSettingsSnapshot {
    /// 破損検知数 (診断スナップショット用)。
    pub fn corrupt_count(&self) -> u32 {
        self.corrupt_keys.len() as u32
    }
}

// ===== 解析ヘルパ =====

/// 設定値文字列を解析する。値は「JSON直列化 (新規キー)」または「素の値
/// (既存backupキー)」。両方試し、`parse`がSomeを返すまで許容する。
fn parse_with<T>(
    raw: &str,
    corrupt: &mut Vec<String>,
    key: &str,
    parse: impl Fn(&str) -> Option<T>,
) -> Option<T> {
    // JSON文字列形式 (`"warn"`) のアンクォート
    let unquoted = serde_json::from_str::<String>(raw)
        .ok()
        .unwrap_or_else(|| raw.to_string());
    match parse(&unquoted) {
        Some(value) => Some(value),
        None => {
            tracing::warn!(
                key = key,
                "invalid app_settings value; falling back to default"
            );
            corrupt.push(key.to_string());
            None
        }
    }
}

// ===== 読み取り =====

/// 全アプリ設定を1 readerパスで読み取る。破損値は既定値+warnで継続。
pub fn load_app_settings(db: &Db) -> AppSettingsSnapshot {
    db.with_reader(|repos| {
        let mut corrupt = Vec::new();
        let get = |key: &str| -> Option<String> { repos.setting_get(key).ok().flatten() };

        let general = GeneralSettings {
            log_level: get(SETTING_GENERAL_LOG_LEVEL)
                .and_then(|raw| {
                    parse_with(
                        &raw,
                        &mut corrupt,
                        SETTING_GENERAL_LOG_LEVEL,
                        LogLevel::parse,
                    )
                })
                .unwrap_or(LogLevel::Info),
        };
        let cache = CacheSettings {
            thumbs_max_bytes: get(SETTING_CACHE_THUMBS_MAX_BYTES)
                .and_then(|raw| {
                    parse_with(&raw, &mut corrupt, SETTING_CACHE_THUMBS_MAX_BYTES, |v| {
                        v.parse::<u64>()
                            .ok()
                            .filter(|n| (MIN_THUMBS_MAX_BYTES..=MAX_THUMBS_MAX_BYTES).contains(n))
                    })
                })
                .unwrap_or(DEFAULT_THUMBS_MAX_BYTES),
        };
        let backup = BackupSettingsView {
            auto_full_db: get(SETTING_BACKUP_AUTO_FULL_DB)
                .map(|raw| {
                    // 既存の素の形式 ("off" 等)。無効値は既定 (Off) + 破損扱い。
                    let freq = AutoBackupFrequency::parse(&raw);
                    if freq.as_str() != raw {
                        tracing::warn!(
                            key = SETTING_BACKUP_AUTO_FULL_DB,
                            "invalid app_settings value; falling back to default"
                        );
                        corrupt.push(SETTING_BACKUP_AUTO_FULL_DB.to_string());
                    }
                    freq
                })
                .unwrap_or(AutoBackupFrequency::Off),
            retention_generations: get(SETTING_BACKUP_RETENTION)
                .and_then(|raw| {
                    parse_with(&raw, &mut corrupt, SETTING_BACKUP_RETENTION, |v| {
                        v.parse::<u32>().ok().filter(|n| (1..=100).contains(n))
                    })
                })
                .unwrap_or(DEFAULT_BACKUP_RETENTION as u32),
        };
        let fts_last_rebuild_at = get(SETTING_FTS_LAST_REBUILD);
        let settings_version = get(SETTING_SETTINGS_VERSION)
            .and_then(|raw| {
                parse_with(&raw, &mut corrupt, SETTING_SETTINGS_VERSION, |v| {
                    v.parse::<u64>()
                        .ok()
                        .filter(|v| *v <= SETTINGS_SCHEMA_VERSION)
                })
            })
            .unwrap_or(SETTINGS_SCHEMA_VERSION);

        Ok(AppSettingsSnapshot {
            general,
            cache,
            backup,
            fts_last_rebuild_at,
            settings_version,
            corrupt_keys: corrupt,
        })
    })
    // with_readerはRepoErrorを返す規約のため、ここでは読み取り失敗も
    // 既定値スナップショットへ落とす (設定が原因でアプリを落とさない)。
    .unwrap_or_else(|e| {
        tracing::warn!(error = %e, "app_settings load failed; using defaults");
        AppSettingsSnapshot {
            general: GeneralSettings::default(),
            cache: CacheSettings::default(),
            backup: BackupSettingsView::default(),
            fts_last_rebuild_at: None,
            settings_version: SETTINGS_SCHEMA_VERSION,
            corrupt_keys: Vec::new(),
        }
    })
}

// ===== 書き込み =====

/// `settings.version` 行が無ければ現行versionを書く (他の設定書き込みに
/// 併せて呼ぶ — 読み取りだけで書かない)。
fn ensure_settings_version(repos: &crate::repos::SqliteRepos<'_>) -> Result<(), RepoError> {
    if repos.setting_get(SETTING_SETTINGS_VERSION)?.is_none() {
        repos.setting_set(
            SETTING_SETTINGS_VERSION,
            &SETTINGS_SCHEMA_VERSION.to_string(),
        )?;
    }
    Ok(())
}

/// ログレベルを保存する (JSON文字列形式)。
pub fn set_log_level(db: &Db, level: LogLevel) -> Result<(), RepoError> {
    db.with_writer(|repos| {
        let encoded =
            serde_json::to_string(level.as_str()).map_err(|e| RepoError::Db(e.to_string()))?;
        repos.setting_set(SETTING_GENERAL_LOG_LEVEL, &encoded)?;
        ensure_settings_version(repos)
    })
}

/// サムネイルキャッシュ上限を保存する (範囲外は拒否)。
pub fn set_thumbs_max_bytes(db: &Db, bytes: u64) -> Result<(), RepoError> {
    if !(MIN_THUMBS_MAX_BYTES..=MAX_THUMBS_MAX_BYTES).contains(&bytes) {
        return Err(RepoError::Constraint(format!(
            "サムネイルキャッシュ上限は{}MiB〜{}TiBで指定してください",
            MIN_THUMBS_MAX_BYTES / (1024 * 1024),
            MAX_THUMBS_MAX_BYTES / (1024 * 1024 * 1024 * 1024),
        )));
    }
    db.with_writer(|repos| {
        repos.setting_set(SETTING_CACHE_THUMBS_MAX_BYTES, &bytes.to_string())?;
        ensure_settings_version(repos)
    })
}

/// FTS最終再構築時刻を現在時刻で記録する (fts_rebuild/repair呼び出し側から)。
pub fn set_fts_last_rebuild_now(db: &Db) -> Result<(), RepoError> {
    db.with_writer(|repos| {
        repos.setting_set(SETTING_FTS_LAST_REBUILD, &now_utc_rfc3339())?;
        ensure_settings_version(repos)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DbOptions;

    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "gento-db-settings-{tag}-{}-{}",
                std::process::id(),
                crate::ids::now_utc_compact()
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

    fn db(tag: &str) -> (TempDir, crate::Db) {
        let tmp = TempDir::new(tag);
        let db = crate::Db::open_with(tmp.0.join("db.sqlite3"), DbOptions::default()).unwrap();
        (tmp, db)
    }

    fn plant(db: &crate::Db, key: &str, value: &str) {
        db.with_writer(|repos| repos.setting_set(key, value))
            .unwrap();
    }

    #[test]
    fn defaults_match_current_behavior_when_unset() {
        let (_tmp, db) = db("defaults");
        let s = load_app_settings(&db);
        assert_eq!(s.general.log_level, LogLevel::Info);
        // 既定 = 現状挙動 (gento-image cache DEFAULT_MAX_BYTES = 2GiB) と同値。
        // gento-dbはgento-imageに依存しないため値を重複定義しており、
        // 同値であることはtauri層のテスト (両crateに依存) で固定する。
        assert_eq!(s.cache.thumbs_max_bytes, 2 * 1024 * 1024 * 1024);
        assert_eq!(s.backup.auto_full_db, AutoBackupFrequency::Off);
        assert_eq!(s.backup.retention_generations, 3);
        assert_eq!(s.fts_last_rebuild_at, None);
        assert_eq!(s.settings_version, SETTINGS_SCHEMA_VERSION);
        assert!(s.corrupt_keys.is_empty());
        assert_eq!(s.corrupt_count(), 0);
    }

    #[test]
    fn roundtrip_log_level_and_thumbs_bytes() {
        let (_tmp, db) = db("roundtrip");
        set_log_level(&db, LogLevel::Debug).unwrap();
        set_thumbs_max_bytes(&db, 512 * 1024 * 1024).unwrap();
        let s = load_app_settings(&db);
        assert_eq!(s.general.log_level, LogLevel::Debug);
        assert_eq!(s.cache.thumbs_max_bytes, 512 * 1024 * 1024);
        assert!(s.corrupt_keys.is_empty());
        // settings.version行が自動作成される
        let raw = db
            .with_reader(|repos| repos.setting_get(SETTING_SETTINGS_VERSION))
            .unwrap()
            .unwrap();
        assert_eq!(raw, "1");
    }

    #[test]
    fn corrupted_values_fall_back_to_defaults_and_are_counted() {
        let (_tmp, db) = db("corrupt");
        plant(&db, SETTING_GENERAL_LOG_LEVEL, "{broken json"); // JSONでも素でも不可
        plant(&db, SETTING_CACHE_THUMBS_MAX_BYTES, "not-a-number");
        plant(&db, SETTING_BACKUP_RETENTION, "0"); // 範囲外
        plant(&db, SETTING_BACKUP_AUTO_FULL_DB, "hourly"); // 不正頻度
        plant(&db, SETTING_SETTINGS_VERSION, "99"); // 未来version

        let s = load_app_settings(&db);
        assert_eq!(s.general.log_level, LogLevel::Info, "破損時の既定");
        assert_eq!(s.cache.thumbs_max_bytes, DEFAULT_THUMBS_MAX_BYTES);
        assert_eq!(s.backup.retention_generations, 3);
        assert_eq!(s.backup.auto_full_db, AutoBackupFrequency::Off);
        assert_eq!(s.settings_version, SETTINGS_SCHEMA_VERSION);
        // 破損キーが全件検知される (診断の設定破損検知数)
        for key in [
            SETTING_GENERAL_LOG_LEVEL,
            SETTING_CACHE_THUMBS_MAX_BYTES,
            SETTING_BACKUP_RETENTION,
            SETTING_BACKUP_AUTO_FULL_DB,
            SETTING_SETTINGS_VERSION,
        ] {
            assert!(s.corrupt_keys.contains(&key.to_string()), "missing {key}");
        }
        assert_eq!(s.corrupt_count(), 5);
    }

    #[test]
    fn legacy_backup_formats_still_parse() {
        // 既存コマンド (backup_settings_set) が書く素の形式との互換。
        let (_tmp, db) = db("legacy");
        plant(&db, SETTING_BACKUP_AUTO_FULL_DB, "weekly");
        plant(&db, SETTING_BACKUP_RETENTION, "5");
        let s = load_app_settings(&db);
        assert_eq!(s.backup.auto_full_db, AutoBackupFrequency::Weekly);
        assert_eq!(s.backup.retention_generations, 5);
        assert!(s.corrupt_keys.is_empty());
    }

    #[test]
    fn json_forms_parse() {
        let (_tmp, db) = db("json");
        plant(&db, SETTING_GENERAL_LOG_LEVEL, "\"warn\"");
        plant(&db, SETTING_CACHE_THUMBS_MAX_BYTES, "1073741824");
        let s = load_app_settings(&db);
        assert_eq!(s.general.log_level, LogLevel::Warn);
        assert_eq!(s.cache.thumbs_max_bytes, 1024 * 1024 * 1024);
        assert!(s.corrupt_keys.is_empty());
    }

    #[test]
    fn out_of_range_thumbs_bytes_rejected_on_set_and_counted_on_load() {
        let (_tmp, db) = db("range");
        let err = set_thumbs_max_bytes(&db, 1024).unwrap_err();
        assert!(err.to_string().contains("MiB"), "{err}");
        // 既存DBに範囲外値が混入していた場合も既定へフォールバック
        plant(&db, SETTING_CACHE_THUMBS_MAX_BYTES, "99999999999999999");
        let s = load_app_settings(&db);
        assert_eq!(s.cache.thumbs_max_bytes, DEFAULT_THUMBS_MAX_BYTES);
        assert!(s
            .corrupt_keys
            .contains(&SETTING_CACHE_THUMBS_MAX_BYTES.to_string()));
    }

    #[test]
    fn fts_last_rebuild_roundtrip() {
        let (_tmp, db) = db("fts");
        assert_eq!(load_app_settings(&db).fts_last_rebuild_at, None);
        set_fts_last_rebuild_now(&db).unwrap();
        let at = load_app_settings(&db)
            .fts_last_rebuild_at
            .expect("recorded");
        assert!(at.ends_with('Z') && at.len() == 24, "RFC3339形式: {at}");
    }

    #[test]
    fn log_level_parse_case_insensitive() {
        assert_eq!(LogLevel::parse("INFO"), Some(LogLevel::Info));
        assert_eq!(LogLevel::parse(" debug "), Some(LogLevel::Debug));
        assert_eq!(LogLevel::parse("trace"), None);
        assert_eq!(LogLevel::parse(""), None);
    }
}
