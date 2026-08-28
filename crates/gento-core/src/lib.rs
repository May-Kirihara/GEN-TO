//! GEN-TO コアドメイン型。Tauriに依存してはならない
//! (docs/implementation-plan.md §5.1)。

pub mod catalog;

use std::fmt;
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

/// `health` commandが返すバックエンド健全性スナップショット。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthInfo {
    pub app_name: String,
    pub version: String,
    /// データセットディレクトリのパス (UI表示用の絶対パス文字列)。
    /// 設定されていない場合は `null`。
    pub dataset_path: Option<String>,
    /// `dataset_path` が指すディレクトリが存在するかどうか。
    /// 不存在でも起動は継続する (docs/implementation-plan.md §4.1)。
    pub dataset_exists: bool,
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
            dataset_path: Some("/tmp/sample/dataset".to_string()),
            dataset_exists: true,
            timestamp_ms: 1_759_000_000_000,
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "app_name": "GEN-TO 技術検証プロトタイプ",
                "version": "0.1.0",
                "dataset_path": "/tmp/sample/dataset",
                "dataset_exists": true,
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
            dataset_path: None,
            dataset_exists: false,
            timestamp_ms: 1,
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["dataset_path"], serde_json::Value::Null);
        assert_eq!(json["dataset_exists"], false);
    }
}
