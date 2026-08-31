//! コレクション (manual / smart) のrepository抽象 (MVP WP-6)。
//!
//! data-model §7 / spec §7:
//! - コレクションはファイルを移動しない仮想分類 (fs無操作)
//! - 手動 (manual) は `CollectionItem` を介したAsset紐付け。任意並び順
//!   (position) とコレクション内メモ (note) を持つ
//! - スマート (smart) は構造化検索条件JSON (`query_json`) を保持する。
//!   **SQL文そのものは保存しない** — 条件からSQLを構築するのは
//!   `gento-db` 実装の責務 (パラメータバインドのみ。文字列連結禁止)
//!
//! # query_json schema (WP-6時点の確定形)
//!
//! ```json
//! {
//!   "conditions": [
//!     { "type": "rating_ge",  "value": 4 },
//!     { "type": "cull_state", "value": "picked" },
//!     { "type": "favorite",   "value": true },
//!     { "type": "has_tag",    "name": "sky" },
//!     { "type": "mime_type",  "value": "image/png" }
//!   ]
//! }
//! ```
//!
//! - トップレベルは `conditions` 配列のみ (未知キーは拒否。WP-8の検索AST
//!   接続時にversion付きで拡張する)
//! - 条件は**AND結合** (順序保存)。空配列 = 全Assetに一致
//! - `rating_ge`: 整数 0..=5
//! - `cull_state`: `"unselected" | "picked" | "rejected"`
//! - `favorite`: 真偽値
//! - `has_tag`: タグ名文字列 (保存時に[`crate::metadata::normalize_tag_name`]
//!   で正規化。空は拒否)
//! - `mime_type`: 非空文字列 (assets.mime_type との完全一致)
//! - 未知の条件型・条件ごとの余分/欠落フィールドは拒否
//!   (保存済み条件の意味が静的に確定するため、WP-8拡張時に旧データを
//!   機械的に判定できる)
//!
//! # position規約 (手動コレクション)
//!
//! positionは**常時 0開始の連番**として維持する (どの操作後も
//! `0,1,…,len-1`)。追加は末尾 (追加前の件数)、削除は残項目の詰め、
//! 並び替えは全項目の順序を送って 0 から振り直す。部分更新で穴が
//! 開く設計にはしない (「任意並び順」の正本を一意に保つため)。

use std::fmt;

use serde::Serialize;

use crate::metadata::normalize_tag_name;
use crate::repo::RepoError;

/// コレクション種別 (collections.kind)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionKind {
    Manual,
    Smart,
}

impl CollectionKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            CollectionKind::Manual => "manual",
            CollectionKind::Smart => "smart",
        }
    }

    /// DB格納値のパース。不正値はNone。
    pub fn from_db_value(s: &str) -> Option<Self> {
        match s {
            "manual" => Some(CollectionKind::Manual),
            "smart" => Some(CollectionKind::Smart),
            _ => None,
        }
    }
}

impl fmt::Display for CollectionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// コレクション (collections) のレコード。`item_count` は結合集計
/// (manual=登録項目数 / smart=現在の評価結果件数。list時点のスナップショット)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionRecord {
    pub id: i64,
    pub uuid: String,
    pub name: String,
    pub kind: CollectionKind,
    /// smartのみ。正本のJSON文字列 (本モジュールのschema参照)。
    pub query_json: Option<String>,
    /// manual=collection_items件数 / smart=評価件数。
    pub item_count: u64,
    pub created_at: String,
    pub updated_at: String,
}

/// 手動コレクションの項目 (collection_items) のレコード。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionItemRecord {
    pub collection_id: i64,
    pub asset_id: i64,
    /// 0開始連番 (モジュールdocの規約参照)。
    pub position: u32,
    pub note: Option<String>,
    pub created_at: String,
}

/// スマートコレクションの構造化検索条件 (AND結合)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SmartQuery {
    pub conditions: Vec<SmartCondition>,
}

/// 条件型 (schemaはモジュールdoc参照)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SmartCondition {
    /// 星評価が value 以上。
    RatingGe { value: u8 },
    /// 選別状態が value と一致。
    CullState { value: SmartCullValue },
    /// お気に入りが value と一致。
    Favorite { value: bool },
    /// タグ (正規化名) を持つ。`name` は正規化済み。
    HasTag { name: String },
    /// MIME型が value と完全一致。
    MimeType { value: String },
}

/// `cull_state` 条件の値 (repo::TriageStateと同じ値域)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SmartCullValue {
    Unselected,
    Picked,
    Rejected,
}

impl SmartCullValue {
    pub const fn as_str(self) -> &'static str {
        match self {
            SmartCullValue::Unselected => "unselected",
            SmartCullValue::Picked => "picked",
            SmartCullValue::Rejected => "rejected",
        }
    }
}

impl From<crate::repo::TriageState> for SmartCullValue {
    fn from(value: crate::repo::TriageState) -> Self {
        match value {
            crate::repo::TriageState::Unselected => SmartCullValue::Unselected,
            crate::repo::TriageState::Picked => SmartCullValue::Picked,
            crate::repo::TriageState::Rejected => SmartCullValue::Rejected,
        }
    }
}

/// query_jsonの解析失敗。メッセージは保存・表示にそのまま使える日本語。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmartQueryError(pub String);

impl fmt::Display for SmartQueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SmartQueryError {}

impl SmartQuery {
    /// 正規形JSON文字列へ書き出す (保存形式)。
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("SmartQuery serialization is infallible")
    }

    /// 保存前の正規化: `has_tag` の名前を[`normalize_tag_name`]で正規化する
    /// (空になったらエラー)。保存済みquery_jsonが常に正規形であることを
    /// 保証する — repoの書き込み経路で必ず通す。
    pub fn canonicalize(&self) -> Result<SmartQuery, SmartQueryError> {
        let mut conditions = Vec::with_capacity(self.conditions.len());
        for condition in &self.conditions {
            let normalized = match condition {
                SmartCondition::HasTag { name } => {
                    let normalized = normalize_tag_name(name);
                    if normalized.is_empty() {
                        return Err(SmartQueryError("has_tag の name が空です".to_string()));
                    }
                    SmartCondition::HasTag { name: normalized }
                }
                other => other.clone(),
            };
            conditions.push(normalized);
        }
        Ok(SmartQuery { conditions })
    }

    /// 保存形式JSONを厳密に解析する (schemaはモジュールdoc参照)。
    /// 不正JSON・未知条件型・余分/欠落フィールド・値域外は
    /// [`SmartQueryError`] (日本語メッセージ)。
    pub fn parse(json: &str) -> Result<Self, SmartQueryError> {
        let value: serde_json::Value = serde_json::from_str(json)
            .map_err(|e| SmartQueryError(format!("検索条件のJSONが不正です: {e}")))?;
        let err = |msg: &str| SmartQueryError(msg.to_string());
        let object = value
            .as_object()
            .ok_or_else(|| err("検索条件はオブジェクトである必要があります"))?;
        if let Some(key) = object.keys().find(|k| k.as_str() != "conditions") {
            return Err(err(&format!("検索条件に未知のフィールドがあります: {key}")));
        }
        let conditions_value = object
            .get("conditions")
            .ok_or_else(|| err("検索条件に conditions が必要です"))?;
        let array = conditions_value
            .as_array()
            .ok_or_else(|| err("conditions は配列である必要があります"))?;
        let mut conditions = Vec::with_capacity(array.len());
        for entry in array {
            conditions.push(SmartCondition::parse(entry)?);
        }
        Ok(Self { conditions })
    }
}

impl SmartCondition {
    fn parse(value: &serde_json::Value) -> Result<Self, SmartQueryError> {
        let err = |msg: String| SmartQueryError(msg);
        let object = value
            .as_object()
            .ok_or_else(|| err("条件はオブジェクトである必要があります".to_string()))?;
        let type_value = object
            .get("type")
            .ok_or_else(|| err("条件に type が必要です".to_string()))?;
        let type_str = type_value
            .as_str()
            .ok_or_else(|| err("条件の type は文字列である必要があります".to_string()))?;
        let expect_fields = |object: &serde_json::Map<String, serde_json::Value>,
                             expected: &[&str]|
         -> Result<(), SmartQueryError> {
            for key in object.keys() {
                if key != "type" && !expected.contains(&key.as_str()) {
                    return Err(SmartQueryError(format!(
                        "条件 {type_str} に未知のフィールドがあります: {key}"
                    )));
                }
            }
            for expected_key in expected {
                if !object.contains_key(*expected_key) {
                    return Err(SmartQueryError(format!(
                        "条件 {type_str} に {expected_key} が必要です"
                    )));
                }
            }
            Ok(())
        };
        match type_str {
            "rating_ge" => {
                expect_fields(object, &["value"])?;
                let raw = object.get("value").unwrap();
                let rating = raw.as_u64().ok_or_else(|| {
                    err("rating_ge の value は整数である必要があります".to_string())
                })?;
                if rating > 5 {
                    return Err(err(
                        "rating_ge の value は0〜5で指定してください".to_string()
                    ));
                }
                Ok(SmartCondition::RatingGe {
                    value: rating as u8,
                })
            }
            "cull_state" => {
                expect_fields(object, &["value"])?;
                let raw = object.get("value").unwrap().as_str().ok_or_else(|| {
                    err("cull_state の value は文字列である必要があります".to_string())
                })?;
                let parsed = match raw {
                    "unselected" => SmartCullValue::Unselected,
                    "picked" => SmartCullValue::Picked,
                    "rejected" => SmartCullValue::Rejected,
                    other => {
                        return Err(err(format!(
                            "cull_state の value が不正です: {other} (unselected|picked|rejected)"
                        )))
                    }
                };
                Ok(SmartCondition::CullState { value: parsed })
            }
            "favorite" => {
                expect_fields(object, &["value"])?;
                let raw = object.get("value").unwrap().as_bool().ok_or_else(|| {
                    err("favorite の value は真偽値である必要があります".to_string())
                })?;
                Ok(SmartCondition::Favorite { value: raw })
            }
            "has_tag" => {
                expect_fields(object, &["name"])?;
                let raw = object.get("name").unwrap().as_str().ok_or_else(|| {
                    err("has_tag の name は文字列である必要があります".to_string())
                })?;
                let normalized = normalize_tag_name(raw);
                if normalized.is_empty() {
                    return Err(err("has_tag の name が空です".to_string()));
                }
                Ok(SmartCondition::HasTag { name: normalized })
            }
            "mime_type" => {
                expect_fields(object, &["value"])?;
                let raw = object.get("value").unwrap().as_str().ok_or_else(|| {
                    err("mime_type の value は文字列である必要があります".to_string())
                })?;
                if raw.is_empty() {
                    return Err(err("mime_type の value が空です".to_string()));
                }
                Ok(SmartCondition::MimeType {
                    value: raw.to_string(),
                })
            }
            other => Err(err(format!(
                "不明な条件型です: {other} (rating_ge|cull_state|favorite|has_tag|mime_type)"
            ))),
        }
    }
}

/// コレクションのrepository。
///
/// 同名コレクションは**許可**する (识别はuuid。UI側で必要なら一覧から
/// 重複を表示してユーザーに判断させる — WP-6の確定方針)。
pub trait CollectionRepo {
    /// 手動コレクションを作成する。名前はtrimのみ (空は[`RepoError::Constraint`])。
    fn create_manual_collection(&self, name: &str) -> Result<CollectionRecord, RepoError>;

    /// スマートコレクションを作成する (query_json保存)。queryは正規形JSONへ
    /// 書き出して格納する。
    fn create_smart_collection(
        &self,
        name: &str,
        query: &SmartQuery,
    ) -> Result<CollectionRecord, RepoError>;

    /// WP-8: 検索AST (gento-search) を正本とするスマートコレクションを作る。
    /// `search_json` は gento-search の保存形式 (version 2)。AST型は
    /// gento-search crateにあるため、ここでは検証済み正規JSON文字列を
    /// 受け取る (検証・正規化は実装層がgento-search経由で行う)。
    /// 旧最小条件形式での新規書き込みは不可 (読み取り互換のみ)。
    fn create_smart_collection_search(
        &self,
        name: &str,
        search_json: &str,
    ) -> Result<CollectionRecord, RepoError>;

    /// 改名する。存在しないIDは[`RepoError::NotFound`]。
    fn rename_collection(
        &self,
        collection_id: i64,
        name: &str,
    ) -> Result<CollectionRecord, RepoError>;

    /// 削除する (itemsごと。ON DELETE CASCADE)。fsへは一切触れない。
    /// 存在しないIDは[`RepoError::NotFound`]。
    fn delete_collection(&self, collection_id: i64) -> Result<(), RepoError>;

    /// 一覧 (作成順。item_count付き。smartのcountは評価結果の現在値)。
    fn list_collections(&self) -> Result<Vec<CollectionRecord>, RepoError>;

    /// 主キーで引く。
    fn get_collection(&self, collection_id: i64) -> Result<Option<CollectionRecord>, RepoError>;

    /// 項目を末尾へ追加する (asset複数可。1トランザクション)。
    /// 既に含まれるassetは冪等スキップ (position保持)。
    /// 不在asset・smartコレクションへの追加はエラー (ロールバック)。
    fn add_collection_items(
        &self,
        collection_id: i64,
        asset_ids: &[i64],
    ) -> Result<Vec<CollectionItemRecord>, RepoError>;

    /// 項目を1件削除し、残項目のpositionを詰める (0開始連番を維持)。
    /// 存在しない組は[`RepoError::NotFound`]。
    fn remove_collection_item(&self, collection_id: i64, asset_id: i64) -> Result<(), RepoError>;

    /// 並び順を更新する (ドラッグ後の**全項目の順序**を受ける)。
    /// `asset_ids` が現在の項目集合と一致しない場合は[`RepoError::Constraint`]
    /// (部分更新不可)。positionを 0 から振り直す。
    fn reorder_collection(
        &self,
        collection_id: i64,
        asset_ids: &[i64],
    ) -> Result<Vec<CollectionItemRecord>, RepoError>;

    /// コレクション内一覧 (position順・ページング)。戻り値は (ページ, 全件数)。
    fn list_collection_items(
        &self,
        collection_id: i64,
        offset: u32,
        limit: u32,
    ) -> Result<(Vec<CollectionItemRecord>, u64), RepoError>;

    /// 項目メモを設定する (None でクリア)。存在しない組は[`RepoError::NotFound`]。
    fn set_collection_item_note(
        &self,
        collection_id: i64,
        asset_id: i64,
        note: Option<&str>,
    ) -> Result<CollectionItemRecord, RepoError>;

    /// スマートコレクションの条件と名前を更新する。
    /// manualコレクションに対する呼び出しは[`RepoError::Constraint`]。
    fn update_smart_collection(
        &self,
        collection_id: i64,
        name: &str,
        query: &SmartQuery,
    ) -> Result<CollectionRecord, RepoError>;

    /// WP-8: 検索AST正本のスマートコレクションを更新する
    /// (形式は[`CollectionRepo::create_smart_collection_search`]と同じ)。
    fn update_smart_collection_search(
        &self,
        collection_id: i64,
        name: &str,
        search_json: &str,
    ) -> Result<CollectionRecord, RepoError>;

    /// スマートコレクションを評価する (条件→SQL。パラメータバインドのみ)。
    /// 戻り値は (asset行ID列 (id昇順), 全件数)。manualコレクションは
    /// [`RepoError::Constraint`]。保存済みquery_jsonが不正な場合は
    /// [`RepoError::Constraint`]。
    fn evaluate_smart_collection(
        &self,
        collection_id: i64,
        offset: u32,
        limit: u32,
    ) -> Result<(Vec<i64>, u64), RepoError>;

    /// 表示用: `asset_ids` (順序保持) ごとに代表File (present・最小file id)
    /// とAssetの組を返す。presentなFileを持たないassetは結果から除外される。
    /// 呼び出し側はページ規模のリストを渡すこと (IN句をチャンク分割するが、
    /// 全件取得が意図なら素直に大きいリストでよい)。
    fn list_present_files_of_assets(
        &self,
        asset_ids: &[i64],
    ) -> Result<Vec<(crate::repo::FileRecord, crate::repo::AssetRecord)>, RepoError>;
}

/// コレクション名の検証 (trim後空は不可)。
pub fn validate_collection_name(name: &str) -> Result<String, RepoError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(RepoError::Constraint(
            "collection name is empty after trim".to_string(),
        ));
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ===== schema検証 (厳密パーサ) =====

    #[test]
    fn parse_accepts_all_condition_types() {
        let json = r#"{"conditions":[
            {"type":"rating_ge","value":4},
            {"type":"cull_state","value":"picked"},
            {"type":"favorite","value":true},
            {"type":"has_tag","name":"Sky"},
            {"type":"mime_type","value":"image/png"}
        ]}"#;
        let query = SmartQuery::parse(json).unwrap();
        assert_eq!(
            query.conditions,
            vec![
                SmartCondition::RatingGe { value: 4 },
                SmartCondition::CullState {
                    value: SmartCullValue::Picked
                },
                SmartCondition::Favorite { value: true },
                // has_tagは正規化されて保持される
                SmartCondition::HasTag {
                    name: "sky".to_string()
                },
                SmartCondition::MimeType {
                    value: "image/png".to_string()
                },
            ]
        );
    }

    #[test]
    fn parse_rejects_invalid_json() {
        assert!(SmartQuery::parse("not json").is_err());
        assert!(SmartQuery::parse("[]").is_err()); // トップレベルはオブジェクト
        assert!(SmartQuery::parse("{}").is_err()); // conditions必須
        assert!(SmartQuery::parse(r#"{"conditions":{}}"#).is_err()); // 配列であること
    }

    #[test]
    fn parse_rejects_unknown_condition_type_and_fields() {
        // 未知条件型
        let err = SmartQuery::parse(r#"{"conditions":[{"type":"added_after","value":"2026"}]}"#)
            .unwrap_err();
        assert!(err.0.contains("不明な条件型"), "{err:?}");
        // typeなし
        assert!(SmartQuery::parse(r#"{"conditions":[{"value":1}]}"#).is_err());
        // 余分フィールド
        let err = SmartQuery::parse(r#"{"conditions":[{"type":"rating_ge","value":3,"extra":1}]}"#)
            .unwrap_err();
        assert!(err.0.contains("未知のフィールド"), "{err:?}");
        // 欠落フィールド
        assert!(SmartQuery::parse(r#"{"conditions":[{"type":"rating_ge"}]}"#).is_err());
        // トップレベル未知キー (WP-8拡張時にversionで管理する)
        let err = SmartQuery::parse(r#"{"conditions":[],"version":2}"#).unwrap_err();
        assert!(err.0.contains("未知のフィールド"), "{err:?}");
    }

    #[test]
    fn parse_value_domain_boundaries() {
        // rating境界: 0と5はOK・6と負と小数は拒否
        assert!(SmartQuery::parse(r#"{"conditions":[{"type":"rating_ge","value":0}]}"#).is_ok());
        assert!(SmartQuery::parse(r#"{"conditions":[{"type":"rating_ge","value":5}]}"#).is_ok());
        assert!(SmartQuery::parse(r#"{"conditions":[{"type":"rating_ge","value":6}]}"#).is_err());
        assert!(SmartQuery::parse(r#"{"conditions":[{"type":"rating_ge","value":-1}]}"#).is_err());
        assert!(SmartQuery::parse(r#"{"conditions":[{"type":"rating_ge","value":3.5}]}"#).is_err());
        assert!(
            SmartQuery::parse(r#"{"conditions":[{"type":"rating_ge","value":true}]}"#).is_err()
        );
        // cull_state未知値
        assert!(
            SmartQuery::parse(r#"{"conditions":[{"type":"cull_state","value":"gone"}]}"#).is_err()
        );
        // favorite数値拒否
        assert!(SmartQuery::parse(r#"{"conditions":[{"type":"favorite","value":1}]}"#).is_err());
        // has_tag空・空白のみ
        assert!(SmartQuery::parse(r#"{"conditions":[{"type":"has_tag","name":""}]}"#).is_err());
        assert!(SmartQuery::parse(r#"{"conditions":[{"type":"has_tag","name":"  "}]}"#).is_err());
        // mime_type空
        assert!(SmartQuery::parse(r#"{"conditions":[{"type":"mime_type","value":""}]}"#).is_err());
    }

    #[test]
    fn parse_empty_conditions_matches_all() {
        let query = SmartQuery::parse(r#"{"conditions":[]}"#).unwrap();
        assert!(query.conditions.is_empty());
    }

    #[test]
    fn serialize_round_trip_and_canonical_shape() {
        let query = SmartQuery {
            conditions: vec![
                SmartCondition::RatingGe { value: 4 },
                SmartCondition::HasTag {
                    name: "sky".to_string(),
                },
            ],
        };
        let json = query.to_json();
        // 正規形: typeタグ + snake_case
        assert_eq!(
            json,
            r#"{"conditions":[{"type":"rating_ge","value":4},{"type":"has_tag","name":"sky"}]}"#
        );
        assert_eq!(SmartQuery::parse(&json).unwrap(), query);
    }

    #[test]
    fn kind_str_round_trip() {
        assert_eq!(CollectionKind::Manual.as_str(), "manual");
        assert_eq!(CollectionKind::Smart.as_str(), "smart");
        assert_eq!(
            CollectionKind::from_db_value("manual"),
            Some(CollectionKind::Manual)
        );
        assert_eq!(
            CollectionKind::from_db_value("smart"),
            Some(CollectionKind::Smart)
        );
        assert!(CollectionKind::from_db_value("query").is_none());
    }

    #[test]
    fn smart_cull_value_matches_triage_strings() {
        use crate::repo::TriageState;
        assert_eq!(
            SmartCullValue::from(TriageState::Unselected).as_str(),
            "unselected"
        );
        assert_eq!(SmartCullValue::from(TriageState::Picked).as_str(), "picked");
        assert_eq!(
            SmartCullValue::from(TriageState::Rejected).as_str(),
            "rejected"
        );
    }

    #[test]
    fn canonicalize_normalizes_tag_names() {
        let query = SmartQuery {
            conditions: vec![
                SmartCondition::HasTag {
                    name: "  Sky ".to_string(),
                },
                SmartCondition::RatingGe { value: 3 },
            ],
        };
        let canonical = query.canonicalize().unwrap();
        assert_eq!(
            canonical.conditions[0],
            SmartCondition::HasTag {
                name: "sky".to_string()
            }
        );
        // 元は不変
        assert_eq!(
            query.conditions[0].clone(),
            SmartCondition::HasTag {
                name: "  Sky ".to_string()
            }
        );
        // 空タグ名は拒否
        let bad = SmartQuery {
            conditions: vec![SmartCondition::HasTag {
                name: "   ".to_string(),
            }],
        };
        assert!(bad.canonicalize().is_err());
    }

    #[test]
    fn collection_name_validation() {
        assert_eq!(validate_collection_name("  選択  ").unwrap(), "選択");
        assert_eq!(validate_collection_name("a").unwrap(), "a");
        assert!(matches!(
            validate_collection_name("   "),
            Err(RepoError::Constraint(_))
        ));
        assert!(matches!(
            validate_collection_name(""),
            Err(RepoError::Constraint(_))
        ));
    }
}
