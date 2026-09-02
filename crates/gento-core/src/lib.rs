//! GEN-TO コアドメイン型。Tauriに依存してはならない
//! (docs/implementation-plan.md §5.1)。

pub mod assetmeta;
pub mod catalog;
pub mod collections;
pub mod embeddoc;
pub mod file_ops;
pub mod jobs;
pub mod library;
pub mod metadata;
pub mod path_safety;
pub mod repo;

pub use assetmeta::{
    AssetMetadataParsed, AssetMetadataRecord, AssetMetadataRepo, MetadataColumns, ReparseTarget,
};
pub use embeddoc::{
    compose_search_document, compose_search_document_default, SearchDocCaps, SearchDocInput,
};

pub use file_ops::{
    execute_file_op, execute_mkdir, execute_undo, plan_file_op, undo_candidate_of,
    validate_new_name, verify_undo, ConflictInfo, ConflictResolution, FileOpFile, FileOpKind,
    FileOpOutcome, FileOpPlan, FileOpProgress, FileOpRecord, FileOpStatus, FileOpsFs, FileOpsRepo,
    FileRootRef, FileStat, FsOpError, FsOpErrorKind, NewFileOp, PlannedStep, ResolutionSpec,
    StepResult, StepStatus, TokenDto, UndoAction, UndoCandidate, UndoOutcome, UndoRefusal,
    UndoStepResult, TMP_FILE_PREFIX,
};
pub use jobs::{ParsePriorityError, ParseSizeClassError, Priority, SizeClass};
pub use library::{
    decode_library_asset_id, library_asset_id, provisional_file_asset_id, CoalescedChanges,
    FileContentHash, FsChange, HashFileError, LibraryAssetRef, PendingHash, ScanEntry, ScanFs,
    ScanOptions, ScanPhase, ScanProgress, ScanSummary, WalkOutcome, WatchBuffer,
    DEFAULT_SCAN_BATCH_SIZE, PROVISIONAL_FILE_FLAG,
};
pub use path_safety::open_regular_beneath;
pub use repo::{
    AssetRecord, AssetRepo, FileRecord, FileRepo, FileUpsert, HashState, LibraryRootRecord,
    LibraryRootRepo, NewAsset, OsFileId, ParseScanStateError, RepoError, RootSettings, ScanState,
    TriageState,
};

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// アセットの一意識別子。
///
/// IPC DTOでは10進文字列として扱う。u64はJavaScriptの安全な整数範囲
/// (2^53 - 1)を超えるため、JSON数値にするとWebView側で精度が失われる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AssetId(u64);

impl AssetId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u64 {
        self.0
    }
}

impl fmt::Display for AssetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for AssetId {
    type Err = ParseAssetIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // u64::from_str は先頭 '+' を受容するが、DTO文字列は正規形
        // (数字のみ) に限定する。
        if s.starts_with('+') {
            return Err(ParseAssetIdError::new("leading '+' is not allowed"));
        }
        u64::from_str(s)
            .map_err(|e| ParseAssetIdError::new(e.to_string()))
            .map(Self)
    }
}

/// [`AssetId::from_str`] の失敗。
#[derive(Debug)]
pub struct ParseAssetIdError(String);

impl ParseAssetIdError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for ParseAssetIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid asset id: {}", self.0)
    }
}

impl std::error::Error for ParseAssetIdError {}

impl Serialize for AssetId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for AssetId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// GEN-TO IPC DTO契約のAPIバージョン (docs/dev/api-contract.md §15)。
///
/// 互換方針: major (この値の増分) は破壊的変更 (フィールド削除・意味変更)、
/// minorは追加のみ。`health` 応答の `api_version` でフロントが判定する。
pub const GENTO_API_VERSION: u32 = 1;

// APIバージョンは単調増加である必要がある (0や負相当は契約違反)。
const _: () = assert!(GENTO_API_VERSION >= 1);

/// アプリケーション稼働ディレクトリの解決結果 (WP-0)。
///
/// 解決自体はTauriアダプター側 (dirs crate) で行い、コアは注入された
/// 値のみを使う (コアをプラットフォームAPIに依存させない)。
/// MVP-WP-1でSQLite DBは `data_dir` 直下へ置く予定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppDirs {
    /// 永続データ (設定・DB) の置き場所。MVP-WP-1のSQLite DBはここへ置く。
    pub data_dir: PathBuf,
    /// 再生成可能なキャッシュのベースディレクトリ。
    pub cache_dir: PathBuf,
    /// ログ・panic記録の出力先。
    pub log_dir: PathBuf,
}

/// `health` commandが返すバックエンド健全性スナップショット。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthInfo {
    pub app_name: String,
    pub version: String,
    /// IPC DTO契約のAPIバージョン (`GENTO_API_VERSION`)。
    pub api_version: u32,
    /// データセットディレクトリのパス (UI表示用の絶対パス文字列)。
    /// 設定されていない場合は `null`。
    pub dataset_path: Option<String>,
    /// `dataset_path` が指すディレクトリが存在するかどうか。
    /// 不存在でも起動は継続する (docs/implementation-plan.md §4.1)。
    pub dataset_exists: bool,
    /// SQLite DBを開けているか (WP-1。詳細は `db_status` command)。
    pub db_open: bool,
    /// 心拍時刻 (Unixエポックからのミリ秒)。UI側で書式化する。
    /// ミリ秒u64はJS安全整数範囲内に収まる。
    pub timestamp_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Serialize, serde::Deserialize)]
    struct SampleDto {
        id: AssetId,
    }

    #[test]
    fn asset_id_json_round_trip_via_string() {
        let id = AssetId::new(42);
        let json = serde_json::to_value(id).unwrap();
        assert_eq!(json, serde_json::json!("42"));
        let back: AssetId = serde_json::from_value(json).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn asset_id_beyond_js_safe_integer_keeps_precision() {
        // 2^53 + 1 はJavaScriptの安全な整数範囲外。文字列化で回避する。
        let id = AssetId::new(9_007_199_254_740_993);
        let json = serde_json::to_value(id).unwrap();
        assert_eq!(json, serde_json::json!("9007199254740993"));
        let back: AssetId = serde_json::from_value(json).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn asset_id_inside_dto_is_string() {
        let dto = SampleDto {
            id: AssetId::new(7),
        };
        assert_eq!(
            serde_json::to_value(&dto).unwrap(),
            serde_json::json!({ "id": "7" })
        );
    }

    #[test]
    fn asset_id_display_from_str_round_trip() {
        let id = AssetId::new(12_345_678_901_234_567_890);
        let s = id.to_string();
        assert_eq!(s, "12345678901234567890");
        assert_eq!(s.parse::<AssetId>().unwrap(), id);
    }

    #[test]
    fn asset_id_rejects_invalid_strings() {
        for invalid in ["-1", "+1", "abc", "", " 1", "1.0"] {
            assert!(invalid.parse::<AssetId>().is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn health_info_json_shape() {
        let info = HealthInfo {
            app_name: "GEN-TO 技術検証プロトタイプ".to_string(),
            version: "0.1.0".to_string(),
            api_version: GENTO_API_VERSION,
            dataset_path: Some("/tmp/sample/dataset".to_string()),
            dataset_exists: true,
            db_open: true,
            timestamp_ms: 1_759_000_000_000,
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "app_name": "GEN-TO 技術検証プロトタイプ",
                "version": "0.1.0",
                "api_version": GENTO_API_VERSION,
                "dataset_path": "/tmp/sample/dataset",
                "dataset_exists": true,
                "db_open": true,
                "timestamp_ms": 1_759_000_000_000u64
            })
        );
        let back: HealthInfo = serde_json::from_value(json).unwrap();
        assert_eq!(back, info);
    }

    #[test]
    fn health_info_without_dataset_serializes_null() {
        let info = HealthInfo {
            app_name: "GEN-TO 技術検証プロトタイプ".to_string(),
            version: "0.1.0".to_string(),
            api_version: GENTO_API_VERSION,
            dataset_path: None,
            dataset_exists: false,
            db_open: false,
            timestamp_ms: 1,
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["dataset_path"], serde_json::Value::Null);
        assert_eq!(json["dataset_exists"], false);
        assert_eq!(json["db_open"], false);
    }

    // ===== APIバージョン・アプリディレクトリ (WP-0) =====

    #[test]
    fn health_info_rejects_json_without_api_version() {
        // api_versionは必須フィールド: 古い形状 (WP-0前) は拒否される
        let legacy = serde_json::json!({
            "app_name": "GEN-TO 技術検証プロトタイプ",
            "version": "0.1.0",
            "dataset_path": null,
            "dataset_exists": false,
            "timestamp_ms": 1,
        });
        assert!(serde_json::from_value::<HealthInfo>(legacy).is_err());
    }

    #[test]
    fn health_info_rejects_json_without_db_open() {
        // db_openも必須フィールド (WP-1追加): 欠如はerror扱い
        let legacy = serde_json::json!({
            "app_name": "GEN-TO 技術検証プロトタイプ",
            "version": "0.1.0",
            "api_version": 1,
            "dataset_path": null,
            "dataset_exists": false,
            "timestamp_ms": 1,
        });
        assert!(serde_json::from_value::<HealthInfo>(legacy).is_err());
    }

    #[test]
    fn api_version_const_matches_health_field() {
        let info = HealthInfo {
            app_name: "app".to_string(),
            version: "0.1.0".to_string(),
            api_version: GENTO_API_VERSION,
            dataset_path: None,
            dataset_exists: false,
            db_open: false,
            timestamp_ms: 0,
        };
        assert_eq!(info.api_version, GENTO_API_VERSION);
    }

    #[test]
    fn app_dirs_holds_injected_paths() {
        let dirs = AppDirs {
            data_dir: PathBuf::from("/xdg/data/gento"),
            cache_dir: PathBuf::from("/xdg/cache/gento"),
            log_dir: PathBuf::from("/xdg/cache/gento/log"),
        };
        assert_eq!(dirs.data_dir, PathBuf::from("/xdg/data/gento"));
        assert_eq!(dirs.cache_dir, PathBuf::from("/xdg/cache/gento"));
        assert_eq!(dirs.log_dir, PathBuf::from("/xdg/cache/gento/log"));
        let cloned = dirs.clone();
        assert_eq!(cloned, dirs);
    }
}
