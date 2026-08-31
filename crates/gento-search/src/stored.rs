//! スマートコレクション保存形式 (query_json) のWP-8拡張。
//!
//! # 保存形式 (version 2 — WP-8以降の書き込み形式)
//!
//! ```json
//! {
//!   "version": 2,
//!   "query": { "terms": [ {"type":"text","value":"夜景"} ] }
//! }
//! ```
//!
//! `query.terms` は [`crate::SearchQuery`] の直列化と同一形状。
//!
//! # 旧形式 (WP-6の最小条件形式) との互換
//!
//! 旧形式 (`{"conditions":[...]}`) は**読み取り互換**のみ: 読み込み時に
//! ASTへ変換する ([`legacy_to_query`])。新規書き込みはversion 2形式のみ
//! (設計判断: 検索実行経路をAST一本化するため。api-contract §42記録)。

use gento_core::collections::{SmartCondition, SmartQuery};
use serde::Deserialize;

use crate::{CmpOp, FieldCondition, FieldKey, FieldValue, SearchQuery, SearchTerm};

/// 保存済みquery_jsonの解析結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoredSearch {
    /// version 2 (AST正本)。
    Ast(SearchQuery),
    /// WP-6旧形式 (最小条件)。呼び出し側は [`legacy_to_query`] でASTへ
    /// 変換してから検索実行する。
    Legacy(SmartQuery),
}

/// 解析失敗 (日本語メッセージ)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSearchError(pub String);

impl std::fmt::Display for StoredSearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for StoredSearchError {}

#[derive(Deserialize)]
struct StoredV2 {
    version: u32,
    #[serde(default)]
    query: Option<SearchQuery>,
}

/// ASTをversion 2保存JSONへ書き出す。
pub fn to_json(query: &SearchQuery) -> String {
    let wrapper = serde_json::json!({
        "version": 2,
        "query": query,
    });
    wrapper.to_string()
}

/// 保存済みquery_jsonを解析する。旧形式は [`StoredSearch::Legacy`] として
/// 返す (未知キー・不正値は拒否 — WP-6と同じ厳密さ)。
pub fn parse_stored(json: &str) -> Result<StoredSearch, StoredSearchError> {
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| StoredSearchError(format!("検索条件のJSONが不正です: {e}")))?;
    let object = value.as_object().ok_or_else(|| {
        StoredSearchError("検索条件はオブジェクトである必要があります".to_string())
    })?;
    if object.keys().any(|k| k.as_str() == "conditions") {
        // WP-6旧形式はSmartQueryの厳密解析へ委譲
        let legacy = SmartQuery::parse(json).map_err(|e| StoredSearchError(e.0))?;
        return Ok(StoredSearch::Legacy(legacy));
    }
    for key in object.keys() {
        if key.as_str() != "version" && key.as_str() != "query" {
            return Err(StoredSearchError(format!(
                "検索条件に未知のフィールドがあります: {key}"
            )));
        }
    }
    let stored: StoredV2 = serde_json::from_value(value)
        .map_err(|e| StoredSearchError(format!("検索条件のJSONが不正です: {e}")))?;
    if stored.version != 2 {
        return Err(StoredSearchError(format!(
            "対応していない検索条件のバージョンです: {}",
            stored.version
        )));
    }
    match stored.query {
        Some(query) => Ok(StoredSearch::Ast(query)),
        None => Err(StoredSearchError("検索条件に query が必要です".to_string())),
    }
}

/// WP-6旧形式 (最小条件) をASTへ変換する。意味の写像:
///
/// - `rating_ge {v}`  → `rating >= v`
/// - `cull_state {v}` → `cull = v`
/// - `favorite {v}`   → `fav = v`
/// - `has_tag {name}` → `tag = name` (正規化済み)
/// - `mime_type {v}`  → `type = v` (完全一致MIME)
pub fn legacy_to_query(legacy: &SmartQuery) -> SearchQuery {
    let mut terms = Vec::with_capacity(legacy.conditions.len());
    for condition in &legacy.conditions {
        let field = match condition {
            SmartCondition::RatingGe { value } => FieldCondition {
                key: FieldKey::Rating,
                op: CmpOp::Ge,
                value: FieldValue::Number(i64::from(*value)),
            },
            SmartCondition::CullState { value } => FieldCondition {
                key: FieldKey::Cull,
                op: CmpOp::Eq,
                value: FieldValue::Text(value.as_str().to_string()),
            },
            SmartCondition::Favorite { value } => FieldCondition {
                key: FieldKey::Fav,
                op: CmpOp::Eq,
                value: FieldValue::Bool(*value),
            },
            SmartCondition::HasTag { name } => FieldCondition {
                key: FieldKey::Tag,
                op: CmpOp::Eq,
                value: FieldValue::Text(name.clone()),
            },
            SmartCondition::MimeType { value } => FieldCondition {
                key: FieldKey::Type,
                op: CmpOp::Eq,
                value: FieldValue::Text(value.clone()),
            },
        };
        terms.push(SearchTerm::Field(field));
    }
    SearchQuery { terms }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_round_trip() {
        let query = crate::parse("夜景 tag:sky rating:>=4").unwrap().query;
        let json = to_json(&query);
        assert!(json.contains(r#""version":2"#), "{json}");
        match parse_stored(&json).unwrap() {
            StoredSearch::Ast(back) => assert_eq!(back, query),
            other => panic!("expected Ast, got {other:?}"),
        }
    }

    #[test]
    fn legacy_conditions_still_parse() {
        let legacy_json =
            r#"{"conditions":[{"type":"rating_ge","value":4},{"type":"has_tag","name":"sky"}]}"#;
        match parse_stored(legacy_json).unwrap() {
            StoredSearch::Legacy(query) => {
                let ast = legacy_to_query(&query);
                let conditions = ast.field_conditions();
                assert_eq!(conditions.len(), 2);
                assert_eq!(conditions[0].key, FieldKey::Rating);
                assert_eq!(conditions[0].op, CmpOp::Ge);
                assert_eq!(conditions[0].value, FieldValue::Number(4));
                assert_eq!(conditions[1].key, FieldKey::Tag);
                assert_eq!(conditions[1].value, FieldValue::Text("sky".to_string()));
            }
            other => panic!("expected Legacy, got {other:?}"),
        }
    }

    #[test]
    fn legacy_all_condition_types_map() {
        let legacy = SmartQuery {
            conditions: vec![
                SmartCondition::RatingGe { value: 3 },
                SmartCondition::CullState {
                    value: gento_core::collections::SmartCullValue::Picked,
                },
                SmartCondition::Favorite { value: true },
                SmartCondition::HasTag {
                    name: "night".to_string(),
                },
                SmartCondition::MimeType {
                    value: "image/png".to_string(),
                },
            ],
        };
        let ast = legacy_to_query(&legacy);
        let keys: Vec<FieldKey> = ast.field_conditions().iter().map(|c| c.key).collect();
        assert_eq!(
            keys,
            vec![
                FieldKey::Rating,
                FieldKey::Cull,
                FieldKey::Fav,
                FieldKey::Tag,
                FieldKey::Type
            ]
        );
    }

    #[test]
    fn unknown_top_level_field_rejected() {
        let json = r#"{"version":2,"query":{"terms":[]},"extra":1}"#;
        let error = parse_stored(json).unwrap_err();
        assert!(error.0.contains("未知のフィールド"), "{}", error.0);
    }

    #[test]
    fn unsupported_version_rejected() {
        let json = r#"{"version":3,"query":{"terms":[]}}"#;
        let error = parse_stored(json).unwrap_err();
        assert!(error.0.contains("バージョン"), "{}", error.0);
    }

    #[test]
    fn missing_query_rejected() {
        let json = r#"{"version":2}"#;
        assert!(parse_stored(json).is_err());
    }

    #[test]
    fn invalid_legacy_json_rejected() {
        let json = r#"{"conditions":[{"type":"unknown_thing"}]}"#;
        assert!(parse_stored(json).is_err());
    }
}
