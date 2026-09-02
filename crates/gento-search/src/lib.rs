//! 検索AST・parser・FTSクエリコンパイラ (MVP WP-8)。
//!
//! # 構成
//!
//! - [`SearchQuery`] / [`SearchTerm`]: 検索の抽象構文木 (AST)。
//!   GUIフィルタは呼び出し側がASTオブジェクトを組み立てて投入する
//!   (文字列queryとのAND合成は [`SearchQuery::and`] で行う)。
//! - [`parse`]: 検索構文文字列 → AST。失敗は日本語エラー + 位置。
//! - [`TextPlan`]: フリーテキスト条件のFTS5 (trigram) MATCH式と
//!   LIKEフォールバックへの割り振り。
//! - `stored`: スマートコレクション保存形式 (query_json version 2) の
//!   読み書きと、WP-6旧形式 (最小条件) からの変換。
//! - `hybrid`: キーワード×意味の重み付きRRF融合 (WP-12。純Rust)。
//!
//! # 依存方向
//!
//! gento-core ← gento-search ← gento-db / gento-tauri。
//! このcrateはDBへ依存しない (SQLの実行はgento-dbが担う)。
//!
//! # 検索構文 (確定仕様 — docs/dev/api-contract.md §42)
//!
//! ```text
//! 青い髪の少女 tag:night rating:>=3 type:png
//! model:"model-name" steps:>=30 has:workflow
//! added:2026-01-01..2026-06-30 path:"vacation/2026"
//! ```
//!
//! - 空白区切りのterm群 (全部AND)。空クエリ = 全件。
//! - フリーテキスト (引用符なし / `"句"`) は全文一致 (FTS)。
//! - `key:値` = 等価、`key:>=値` `key:<=値` `key:>値` `key:<値` = 比較、
//!   `key:"句"` = 引用値。
//! - keys: `tag` `rating` `cull` `fav` `type` `path` `name` `note` `added`。
//! - WP-9で画像メタデータキーを追加: `model` `sampler` `vae` `lora`
//!   (部分一致) / `steps` (数値比較) / `has:workflow`
//!   (ComfyUI workflow保持。asset_metadata列へクエリ)。

use serde::{Deserialize, Serialize};

/// ハイブリッド検索 (重み付きRRF) のモジュール (WP-12)。
pub mod hybrid;
/// スマートコレクション保存形式 (version 2) のモジュール。
pub mod stored;

pub use hybrid::{clamp_weight, weighted_rrf, FusedRank, RRF_K};

/// パース失敗。メッセージは日本語 (UIへそのまま表示)。`position` は
/// 入力文字列の文字オフセット (0開始)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    pub position: usize,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (位置: {}文字目)", self.message, self.position)
    }
}

impl std::error::Error for ParseError {}

fn err(position: usize, message: impl Into<String>) -> ParseError {
    ParseError {
        message: message.into(),
        position,
    }
}

/// 検索クエリ (term群。全AND結合)。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchQuery {
    pub terms: Vec<SearchTerm>,
}

/// 1つのterm。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SearchTerm {
    /// フリーテキスト (全文一致。引用句を含む)。
    Text { value: String },
    /// 構造化条件。
    Field(FieldCondition),
}

/// 構造化条件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldCondition {
    pub key: FieldKey,
    pub op: CmpOp,
    pub value: FieldValue,
}

/// 条件キー。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKey {
    Tag,
    Rating,
    Cull,
    Fav,
    Type,
    Path,
    Name,
    Note,
    Added,
    /// 生成モデル名 (asset_metadata.generation_model。WP-9)。
    Model,
    /// 生成ステップ数 (asset_metadata.steps。WP-9)。
    Steps,
    /// 特殊保持確認 (`has:workflow`。WP-9)。
    Has,
    /// サンプラー名 (asset_metadata.sampler。WP-9)。
    Sampler,
    /// VAE名 (asset_metadata.vae。WP-9)。
    Vae,
    /// LoRA名 (asset_metadata.generation_extra JSON内。WP-9)。
    Lora,
}

impl FieldKey {
    /// 構文上のキー名。
    pub const fn as_str(self) -> &'static str {
        match self {
            FieldKey::Tag => "tag",
            FieldKey::Rating => "rating",
            FieldKey::Cull => "cull",
            FieldKey::Fav => "fav",
            FieldKey::Type => "type",
            FieldKey::Path => "path",
            FieldKey::Name => "name",
            FieldKey::Note => "note",
            FieldKey::Added => "added",
            FieldKey::Model => "model",
            FieldKey::Steps => "steps",
            FieldKey::Has => "has",
            FieldKey::Sampler => "sampler",
            FieldKey::Vae => "vae",
            FieldKey::Lora => "lora",
        }
    }

    pub fn from_str_opt(value: &str) -> Option<Self> {
        Some(match value {
            "tag" => FieldKey::Tag,
            "rating" => FieldKey::Rating,
            "cull" => FieldKey::Cull,
            "fav" => FieldKey::Fav,
            "type" => FieldKey::Type,
            "path" => FieldKey::Path,
            "name" => FieldKey::Name,
            "note" => FieldKey::Note,
            "added" => FieldKey::Added,
            "model" => FieldKey::Model,
            "steps" => FieldKey::Steps,
            "has" => FieldKey::Has,
            "sampler" => FieldKey::Sampler,
            "vae" => FieldKey::Vae,
            "lora" => FieldKey::Lora,
            _ => return None,
        })
    }
}

/// 比較演算子。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CmpOp {
    Eq,
    Ne,
    Ge,
    Le,
    Gt,
    Lt,
}

impl CmpOp {
    pub const fn as_str(self) -> &'static str {
        match self {
            CmpOp::Eq => "eq",
            CmpOp::Ne => "ne",
            CmpOp::Ge => "ge",
            CmpOp::Le => "le",
            CmpOp::Gt => "gt",
            CmpOp::Lt => "lt",
        }
    }
}

/// 条件値。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldValue {
    /// タグ名・形式・パス・名前・メモ等のテキスト (引用句を含む)。
    Text(String),
    /// 評価 (0..=5) 等の数値。
    Number(i64),
    /// fav 等の真偽値。
    Bool(bool),
    /// added の日付範囲 (どちらか片端のみ可)。`to` はその日を含む。
    DateRange {
        from: Option<IsoDate>,
        to: Option<IsoDate>,
    },
}

/// `YYYY-MM-DD` 日付 (暦日として検証済み)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsoDate {
    pub year: i32,
    pub month: u8,
    pub day: u8,
}

impl IsoDate {
    /// 暦として正しい日付か。うるう年を含む検証。
    pub fn valid(year: i32, month: u8, day: u8) -> bool {
        if !(1..=12).contains(&month) || day == 0 {
            return false;
        }
        let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
        let max = match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 => {
                if leap {
                    29
                } else {
                    28
                }
            }
            _ => return false,
        };
        day <= max
    }

    /// RFC3339比較可能な下限文字列 (その日の00:00)。
    pub fn lower_bound_rfc3339(self) -> String {
        format!(
            "{:04}-{:02}-{:02}T00:00:00.000",
            self.year, self.month, self.day
        )
    }

    /// 翌日の下限文字列 (「その日を含む上限」の排他境界)。
    /// 年跨ぎ・月跨ぎを含む正しい繰り上げ。
    pub fn next_day_lower_bound_rfc3339(self) -> String {
        let leap = (self.year % 4 == 0 && self.year % 100 != 0) || self.year % 400 == 0;
        let max = match self.month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            _ => {
                if leap {
                    29
                } else {
                    28
                }
            }
        };
        if self.day < max {
            return format!(
                "{:04}-{:02}-{:02}T00:00:00.000",
                self.year,
                self.month,
                self.day + 1
            );
        }
        if self.month < 12 {
            return format!("{:04}-{:02}-01T00:00:00.000", self.year, self.month + 1);
        }
        format!("{:04}-01-01T00:00:00.000", self.year + 1)
    }
}

/// パース結果の付帯情報。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParseOutcome {
    pub query: SearchQuery,
    /// 付帯警告 (クエリとしては無視される項目についての注記)。
    pub warnings: Vec<String>,
}

/// 検索構文をパースする。空文字列・空白のみ = 空クエリ (全件)。
pub fn parse(input: &str) -> Result<ParseOutcome, ParseError> {
    let mut outcome = ParseOutcome::default();
    let chars: Vec<char> = input.chars().collect();
    let mut index = 0usize;
    while index < chars.len() {
        // 空白を読み飛ばす
        if chars[index].is_whitespace() {
            index += 1;
            continue;
        }
        let start = index;
        let token = read_token(&chars, &mut index)?;
        let position = start; // 文字オフセット
        match token {
            Token::FreeText(value) => {
                if !value.is_empty() {
                    outcome.query.terms.push(SearchTerm::Text { value });
                }
            }
            Token::Field { key, op, value } => {
                let field_key = FieldKey::from_str_opt(&key)
                    .ok_or_else(|| err(position, format!("不明な検索キーです: {key}")))?;
                let condition = build_condition(position, field_key, op, value)?;
                outcome.query.terms.push(SearchTerm::Field(condition));
            }
        }
    }
    Ok(outcome)
}

/// 字句。`key:値` 形を構造化条件へ、それ以外をフリーテキストへ。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    FreeText(String),
    Field {
        key: String,
        op: FieldOp,
        value: String,
    },
}

/// 構文上のキー側演算子 (`key:>=v` の `>=`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldOp {
    Eq,
    Cmp(CmpOp),
}

/// 空白または引用符で区切られる1トークンを読む。
/// `key:値` / `key:"句"` 形は構造化条件へ、それ以外はフリーテキスト。
fn read_token(chars: &[char], index: &mut usize) -> Result<Token, ParseError> {
    let start = *index;
    let mut pieces: Vec<Piece> = Vec::new();
    while *index < chars.len() && !chars[*index].is_whitespace() {
        if chars[*index] == '"' {
            let phrase = read_quoted(chars, index, start)?;
            pieces.push(Piece::Quote(phrase));
        } else {
            let mut run = String::new();
            while *index < chars.len() && !chars[*index].is_whitespace() && chars[*index] != '"' {
                run.push(chars[*index]);
                *index += 1;
            }
            pieces.push(Piece::Text(run));
        }
    }
    // 先頭が `key:` 形なら構造化条件 (値は残りの全断片の連結)
    if let Some(Piece::Text(first)) = pieces.first() {
        if let Some((key, op_syntax, value_head)) = split_key_prefix(first) {
            let mut value = value_head.to_string();
            for piece in pieces.iter().skip(1) {
                match piece {
                    Piece::Text(text) => value.push_str(text),
                    Piece::Quote(phrase) => value.push_str(phrase),
                }
            }
            let op = match op_syntax {
                None => FieldOp::Eq,
                Some(">=") => FieldOp::Cmp(CmpOp::Ge),
                Some("<=") => FieldOp::Cmp(CmpOp::Le),
                Some(">") => FieldOp::Cmp(CmpOp::Gt),
                Some("<") => FieldOp::Cmp(CmpOp::Lt),
                Some(other) => unreachable!("split_key_prefixは既知演算子のみ返す: {other}"),
            };
            return Ok(Token::Field { key, op, value });
        }
    }
    let mut value = String::new();
    for piece in &pieces {
        match piece {
            Piece::Text(text) => value.push_str(text),
            Piece::Quote(phrase) => value.push_str(phrase),
        }
    }
    Ok(Token::FreeText(value))
}

/// トークンの構成断片 (引用の内外の区別付き)。
enum Piece {
    Text(String),
    Quote(String),
}

/// `key:>=rest` / `key:rest` 形の先頭断片を分解する。
/// keyは空でないASCII英数字のみ。当てはまらなければNone (フリーテキスト)。
fn split_key_prefix(token: &str) -> Option<(String, Option<&'static str>, &str)> {
    let (key, rest) = token.split_once(':')?;
    if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    for symbol in [">=", "<=", ">", "<"] {
        if let Some(value) = rest.strip_prefix(symbol) {
            return Some((key.to_string(), Some(symbol), value));
        }
    }
    Some((key.to_string(), None, rest))
}

/// 引用句を読む (開始の `"` は現在位置)。閉じ忘れはエラー。
fn read_quoted(
    chars: &[char],
    index: &mut usize,
    token_start: usize,
) -> Result<String, ParseError> {
    debug_assert_eq!(chars[*index], '"');
    *index += 1;
    let mut out = String::new();
    while *index < chars.len() {
        let c = chars[*index];
        if c == '"' {
            *index += 1;
            return Ok(out);
        }
        out.push(c);
        *index += 1;
    }
    Err(err(token_start, "引用符 (\") が閉じられていません"))
}

/// キーごとの値検証と条件組み立て。
fn build_condition(
    position: usize,
    key: FieldKey,
    op: FieldOp,
    value: String,
) -> Result<FieldCondition, ParseError> {
    match key {
        FieldKey::Tag => text_condition(key, op, position, value),
        FieldKey::Note => text_condition(key, op, position, value),
        FieldKey::Path => text_condition(key, op, position, value),
        FieldKey::Name => text_condition(key, op, position, value),
        FieldKey::Type => text_condition(key, op, position, value),
        FieldKey::Rating => {
            let op = require_cmp_or_eq(op, position, "rating")?;
            let number: i64 = value
                .parse()
                .map_err(|_| err(position, "rating の値は整数で指定してください (0〜5)"))?;
            if !(0..=5).contains(&number) {
                return Err(err(position, "rating の値は0〜5で指定してください"));
            }
            Ok(FieldCondition {
                key,
                op,
                value: FieldValue::Number(number),
            })
        }
        FieldKey::Cull => {
            let op = require_cmp_or_eq(op, position, "cull")?;
            let parsed = match value.as_str() {
                "unselected" | "未選択" => "unselected",
                "picked" | "採用" => "picked",
                "rejected" | "除外" => "rejected",
                other => {
                    return Err(err(
                        position,
                        format!("cull の値が不正です: {other} (unselected|picked|rejected)"),
                    ))
                }
            };
            Ok(FieldCondition {
                key,
                op,
                value: FieldValue::Text(parsed.to_string()),
            })
        }
        FieldKey::Fav => {
            let op = require_cmp_or_eq(op, position, "fav")?;
            let parsed = match value.as_str() {
                "true" | "yes" => true,
                "false" | "no" => false,
                other => {
                    return Err(err(
                        position,
                        format!("fav の値が不正です: {other} (true|false)"),
                    ))
                }
            };
            Ok(FieldCondition {
                key,
                op,
                value: FieldValue::Bool(parsed),
            })
        }
        FieldKey::Added => {
            require_eq_only(op, position, "added")?;
            let (from, to) = parse_date_range(position, &value)?;
            Ok(FieldCondition {
                key,
                op: CmpOp::Eq,
                value: FieldValue::DateRange { from, to },
            })
        }
        // WP-9: 埋め込み画像メタデータの正規化列 (asset_metadata) への条件。
        FieldKey::Model | FieldKey::Sampler | FieldKey::Vae | FieldKey::Lora => {
            text_condition(key, op, position, value)
        }
        FieldKey::Steps => {
            let op = require_cmp_or_eq(op, position, "steps")?;
            let number: i64 = value
                .parse()
                .map_err(|_| err(position, "steps の値は整数で指定してください"))?;
            Ok(FieldCondition {
                key,
                op,
                value: FieldValue::Number(number),
            })
        }
        FieldKey::Has => {
            // 比較演算子は意味を成さない (テキスト系キーと同様の拒否)。
            let op = match op {
                FieldOp::Eq => CmpOp::Eq,
                FieldOp::Cmp(cmp) => {
                    return Err(err(
                        position,
                        format!(
                            "キー「has」に比較演算子 ({}) は使えません",
                            cmp_to_syntax(cmp)
                        ),
                    ))
                }
            };
            let parsed = match value.as_str() {
                "workflow" => "workflow",
                other => {
                    return Err(err(
                        position,
                        format!("has の値が不正です: {other} (現在は workflow のみ対応)"),
                    ))
                }
            };
            Ok(FieldCondition {
                key,
                op,
                value: FieldValue::Text(parsed.to_string()),
            })
        }
    }
}

/// テキスト系キー (tag/type/path/name/note) の共通組立。
/// 比較演算子 (>=等) は意味を成さないためEq/Neのみ許可する。
fn text_condition(
    key: FieldKey,
    op: FieldOp,
    position: usize,
    value: String,
) -> Result<FieldCondition, ParseError> {
    let op = match op {
        FieldOp::Eq => CmpOp::Eq,
        FieldOp::Cmp(cmp) => {
            return Err(err(
                position,
                format!(
                    "キー「{}」に比較演算子 ({}) は使えません",
                    key.as_str(),
                    cmp_to_syntax(cmp)
                ),
            ))
        }
    };
    if value.is_empty() {
        return Err(err(
            position,
            format!("キー「{}」の値が空です", key.as_str()),
        ));
    }
    Ok(FieldCondition {
        key,
        op,
        value: FieldValue::Text(value),
    })
}

fn require_cmp_or_eq(op: FieldOp, position: usize, key: &str) -> Result<CmpOp, ParseError> {
    let _ = (position, key);
    match op {
        FieldOp::Eq => Ok(CmpOp::Eq),
        FieldOp::Cmp(cmp) => Ok(cmp),
    }
}

/// 範囲構文以外受け付けないキー (added) 用。
fn require_eq_only(op: FieldOp, position: usize, key: &str) -> Result<(), ParseError> {
    match op {
        FieldOp::Eq => Ok(()),
        FieldOp::Cmp(_) => Err(err(
            position,
            format!("キー「{key}」は範囲指定のみです (例: {key}:2026-01-01..2026-06-30)"),
        )),
    }
}

fn cmp_to_syntax(op: CmpOp) -> &'static str {
    match op {
        CmpOp::Eq => "=",
        CmpOp::Ne => "!=",
        CmpOp::Ge => ">=",
        CmpOp::Le => "<=",
        CmpOp::Gt => ">",
        CmpOp::Lt => "<",
    }
}

/// `added` の値 (`YYYY-MM-DD..YYYY-MM-DD` / 片端省略 / 単日) を解析。
fn parse_date_range(
    position: usize,
    value: &str,
) -> Result<(Option<IsoDate>, Option<IsoDate>), ParseError> {
    let parse_one = |text: &str| -> Result<IsoDate, ParseError> {
        let err_here = |msg: String| err(position, msg);
        let parts: Vec<&str> = text.split('-').collect();
        if parts.len() != 3 {
            return Err(err_here(format!(
                "added の日付は YYYY-MM-DD 形式で指定してください: {text}"
            )));
        }
        let year: i32 = parts[0]
            .parse()
            .map_err(|_| err_here("added の年は4桁の整数で指定してください".to_string()))?;
        let month: u8 = parts[1]
            .parse()
            .map_err(|_| err_here("added の月は1〜12の整数で指定してください".to_string()))?;
        let day: u8 = parts[2]
            .parse()
            .map_err(|_| err_here("added の日は1〜31の整数で指定してください".to_string()))?;
        if !IsoDate::valid(year, month, day) {
            return Err(err_here(format!("added の日付が暦上存在しません: {text}")));
        }
        Ok(IsoDate { year, month, day })
    };
    match value.split_once("..") {
        None => {
            // 単日 = その日一日
            let date = parse_one(value)?;
            Ok((Some(date), Some(date)))
        }
        Some((from, to)) => {
            let from = if from.is_empty() {
                None
            } else {
                Some(parse_one(from)?)
            };
            let to = if to.is_empty() {
                None
            } else {
                Some(parse_one(to)?)
            };
            if from.is_none() && to.is_none() {
                return Err(err(
                    position,
                    "added の範囲指定では少なくとも片方の日付を指定してください",
                ));
            }
            Ok((from, to))
        }
    }
}

impl SearchQuery {
    /// 他のクエリの全termをAND合成する (GUIフィルタとの合成用)。
    pub fn and(&mut self, other: &SearchQuery) {
        self.terms.extend(other.terms.iter().cloned());
    }

    /// フリーテキストtermの一覧。
    pub fn text_terms(&self) -> Vec<&str> {
        self.terms
            .iter()
            .filter_map(|term| match term {
                SearchTerm::Text { value } => Some(value.as_str()),
                SearchTerm::Field(_) => None,
            })
            .collect()
    }

    /// 構造化条件の一覧。
    pub fn field_conditions(&self) -> Vec<&FieldCondition> {
        self.terms
            .iter()
            .filter_map(|term| match term {
                SearchTerm::Text { .. } => None,
                SearchTerm::Field(condition) => Some(condition),
            })
            .collect()
    }

    /// 正規形保存JSON (version 2) を作る。
    pub fn to_stored_json(&self) -> String {
        stored::to_json(self)
    }
}

/// フリーテキスト条件の実行計画。
///
/// FTS5 trigramトークナイザは3文字未満のトークンを索引化しないため、
/// 3文字以上のトークンだけで構成される語句はMATCHへ、それ以外
/// (1〜2文字を含む語句) は検索文書テーブルへのLIKEフォールバックへ
/// 割り振る (WP-8設計方針§1。仕様として文書化)。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextPlan {
    /// FTS5 MATCH式 (全MATCH語のAND。trigramで機能する語だけ)。
    /// None = MATCHする語がない。
    pub match_expr: Option<String>,
    /// MATCHへ送る語の引用形 (列フィルタMATCH `col:"語"` 構築用)。
    pub match_terms: Vec<String>,
    /// LIKEフォールバックで部分一致させる語句 (エスケープ前)。
    pub like_terms: Vec<String>,
}

impl TextPlan {
    /// フリーテキストterm群から計画を構築する。
    pub fn compile(terms: &[&str]) -> Self {
        let mut plan = TextPlan::default();
        let mut match_parts: Vec<String> = Vec::new();
        for term in terms {
            if term.is_empty() {
                continue;
            }
            if term_fits_trigram(term) {
                let quoted = quote_fts_string(term);
                match_parts.push(quoted.clone());
                plan.match_terms.push(quoted);
            } else {
                plan.like_terms.push((*term).to_string());
            }
        }
        if !match_parts.is_empty() {
            plan.match_expr = Some(match_parts.join(" "));
        }
        plan
    }

    /// テキスト条件がひとつも無いか (FTS/LIKE参照不要)。
    pub fn is_empty(&self) -> bool {
        self.match_expr.is_none() && self.like_terms.is_empty()
    }
}

/// 語句の全トークン (空白区切り) が3文字以上ならMATCH可能。
fn term_fits_trigram(term: &str) -> bool {
    let mut tokens = term.split_whitespace();
    let mut any = false;
    for token in tokens.by_ref() {
        any = true;
        if token.chars().count() < 3 {
            return false;
        }
    }
    any
}

/// FTS5クエリ構文の引用文字列へ (内部の `"` は `""` にエスケープ)。
fn quote_fts_string(term: &str) -> String {
    format!("\"{}\"", term.replace('"', "\"\""))
}

/// LIKEパターンのエスケープ ( `%` `_` `\` )。ESCAPE '\' と併用する。
pub fn escape_like(term: &str) -> String {
    let mut out = String::with_capacity(term.len() + 4);
    for c in term.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(input: &str) -> ParseOutcome {
        let outcome = parse(input).unwrap();
        assert!(
            outcome.query.terms.iter().all(|term| match term {
                SearchTerm::Text { value } => !value.is_empty(),
                SearchTerm::Field(_) => true,
            }),
            "空のテキストtermは混入しない"
        );
        outcome
    }

    #[test]
    fn empty_query_matches_all() {
        let outcome = parse_ok("");
        assert!(outcome.query.terms.is_empty());
        assert!(outcome.warnings.is_empty());
        let outcome = parse_ok("   \t ");
        assert!(outcome.query.terms.is_empty());
    }

    #[test]
    fn free_text_japanese_and_ascii() {
        let outcome = parse_ok("青い髪の少女");
        assert_eq!(
            outcome.query.terms,
            vec![SearchTerm::Text {
                value: "青い髪の少女".to_string()
            }]
        );
        let outcome = parse_ok("sunset beach 2026");
        assert_eq!(outcome.query.text_terms(), vec!["sunset", "beach", "2026"]);
    }

    #[test]
    fn quoted_phrase_is_single_term() {
        let outcome = parse_ok("\"blue sky\" 夜景");
        assert_eq!(outcome.query.text_terms(), vec!["blue sky", "夜景"]);
    }

    #[test]
    fn spec_example_query() {
        let outcome = parse_ok("青い髪の少女 tag:night rating:>=3 type:png");
        assert_eq!(outcome.query.text_terms(), vec!["青い髪の少女"]);
        let conditions = outcome.query.field_conditions();
        assert_eq!(conditions.len(), 3);
        assert_eq!(
            conditions[0],
            &FieldCondition {
                key: FieldKey::Tag,
                op: CmpOp::Eq,
                value: FieldValue::Text("night".to_string()),
            }
        );
        assert_eq!(
            conditions[1],
            &FieldCondition {
                key: FieldKey::Rating,
                op: CmpOp::Ge,
                value: FieldValue::Number(3),
            }
        );
        assert_eq!(
            conditions[2],
            &FieldCondition {
                key: FieldKey::Type,
                op: CmpOp::Eq,
                value: FieldValue::Text("png".to_string()),
            }
        );
        assert!(outcome.warnings.is_empty());
    }

    #[test]
    fn future_keys_warn_but_do_not_error() {
        // WP-9で model/steps/has は有効化済み。まだ未対応のキーが警告に
        // 流れる経路の検証として、存在しない将来キー相当を直接流し込む。
        // (FUTURE_KEYS機構はWP-9で有効化完了に伴い廃止 — 未知キーは
        // 「不明な検索キー」エラーになる。api-contract §43参照)
        let error = parse("quantum:x").unwrap_err();
        assert!(error.message.contains("不明な検索キー"), "{error}");
    }

    #[test]
    fn wp9_metadata_keys_parse_as_fields() {
        // model/steps/has はWP-9で正式キー化 (embedded画像メタデータ検索)。
        let outcome = parse_ok("model:\"animagine xl\" steps:>=30 has:workflow");
        assert!(
            outcome.warnings.is_empty(),
            "警告は出ない: {warnings:?}",
            warnings = outcome.warnings
        );
        let conditions = outcome.query.field_conditions();
        assert_eq!(conditions.len(), 3);
        assert_eq!(
            conditions[0],
            &FieldCondition {
                key: FieldKey::Model,
                op: CmpOp::Eq,
                value: FieldValue::Text("animagine xl".to_string()),
            }
        );
        assert_eq!(
            conditions[1],
            &FieldCondition {
                key: FieldKey::Steps,
                op: CmpOp::Ge,
                value: FieldValue::Number(30),
            }
        );
        assert_eq!(
            conditions[2],
            &FieldCondition {
                key: FieldKey::Has,
                op: CmpOp::Eq,
                value: FieldValue::Text("workflow".to_string()),
            }
        );
    }

    #[test]
    fn wp9_sampler_vae_lora_keys() {
        let outcome = parse_ok("sampler:\"euler a\" vae:sdxl lora:\"add detail\"");
        let conditions = outcome.query.field_conditions();
        assert_eq!(conditions[0].key, FieldKey::Sampler);
        assert_eq!(conditions[0].value, FieldValue::Text("euler a".to_string()));
        assert_eq!(conditions[1].key, FieldKey::Vae);
        assert_eq!(conditions[2].key, FieldKey::Lora);
        assert_eq!(
            conditions[2].value,
            FieldValue::Text("add detail".to_string())
        );
        // テキスト系キーと同じく比較演算子は不可
        assert!(parse("model:>=x").is_err());
        assert!(parse("sampler:>=x").is_err());
    }

    #[test]
    fn wp9_steps_and_has_validation() {
        assert!(parse("steps:20").is_ok());
        assert!(parse("steps:<50").is_ok());
        let error = parse("steps:abc").unwrap_err();
        assert!(error.message.contains("整数"), "{error}");
        let error = parse("steps:>=x").unwrap_err();
        assert!(error.message.contains("整数"), "{error}");
        // has は workflow のみ (Eq専用)
        let error = parse("has:foo").unwrap_err();
        assert!(error.message.contains("has の値が不正"), "{error}");
        let error = parse("has:>=workflow").unwrap_err();
        assert!(error.message.contains("比較演算子"), "{error}");
    }

    #[test]
    fn unknown_key_is_error_with_position() {
        let error = parse("foo:bar").unwrap_err();
        assert!(error.message.contains("不明な検索キー"), "{error}");
        assert_eq!(error.position, 0);
        let error = parse("tag:x badkey:1").unwrap_err();
        assert_eq!(error.position, 6, "2つ目のtermの位置");
    }

    #[test]
    fn unclosed_quote_is_error() {
        let error = parse("tag:\"night").unwrap_err();
        assert!(error.message.contains("閉じられていません"), "{error}");
        let error = parse("夜景 \"quote").unwrap_err();
        assert!(error.message.contains("閉じられていません"), "{error}");
    }

    #[test]
    fn rating_validation() {
        assert!(parse("rating:3").is_ok());
        assert!(parse("rating:>=3").is_ok());
        assert!(parse("rating:<5").is_ok());
        let error = parse("rating:6").unwrap_err();
        assert!(error.message.contains("0〜5"), "{error}");
        let error = parse("rating:abc").unwrap_err();
        assert!(error.message.contains("整数"), "{error}");
        let error = parse("rating:>=x").unwrap_err();
        assert!(error.message.contains("整数"), "{error}");
    }

    #[test]
    fn cull_and_fav_values() {
        let outcome = parse_ok("cull:picked fav:true");
        let conditions = outcome.query.field_conditions();
        assert_eq!(conditions[0].value, FieldValue::Text("picked".to_string()));
        assert_eq!(conditions[1].value, FieldValue::Bool(true));
        // 日本語値も正規化して受け入れる
        let outcome = parse_ok("cull:採用 fav:no");
        let conditions = outcome.query.field_conditions();
        assert_eq!(conditions[0].value, FieldValue::Text("picked".to_string()));
        assert_eq!(conditions[1].value, FieldValue::Bool(false));
        assert!(parse("cull:unknown").is_err());
        assert!(parse("fav:maybe").is_err());
    }

    #[test]
    fn text_keys_reject_comparison_ops() {
        let error = parse("tag:>=night").unwrap_err();
        assert!(error.message.contains("比較演算子"), "{error}");
        assert!(parse("path:>=x").is_err());
        assert!(parse("name:<y").is_err());
    }

    #[test]
    fn empty_field_value_is_error() {
        // `tag:` 単体は値が空
        let error = parse("tag:").unwrap_err();
        assert!(error.message.contains("値が空"), "{error}");
    }

    #[test]
    fn path_quoted_value() {
        let outcome = parse_ok("path:\"vacation/2026\"");
        assert_eq!(
            outcome.query.field_conditions()[0].value,
            FieldValue::Text("vacation/2026".to_string())
        );
    }

    #[test]
    fn added_date_range_forms() {
        let outcome = parse_ok("added:2026-01-01..2026-06-30");
        let FieldValue::DateRange { from, to } = &outcome.query.field_conditions()[0].value else {
            panic!("date range");
        };
        assert_eq!(
            from.unwrap(),
            IsoDate {
                year: 2026,
                month: 1,
                day: 1
            }
        );
        assert_eq!(
            to.unwrap(),
            IsoDate {
                year: 2026,
                month: 6,
                day: 30
            }
        );

        let outcome = parse_ok("added:2026-01-01..");
        let FieldValue::DateRange { from, to } = &outcome.query.field_conditions()[0].value else {
            panic!("date range");
        };
        assert!(from.is_some() && to.is_none());

        let outcome = parse_ok("added:..2026-06-30");
        let FieldValue::DateRange { from, to } = &outcome.query.field_conditions()[0].value else {
            panic!("date range");
        };
        assert!(from.is_none() && to.is_some());

        // 単日 = その日一日
        let outcome = parse_ok("added:2024-02-29");
        let FieldValue::DateRange { from, to } = &outcome.query.field_conditions()[0].value else {
            panic!("date range");
        };
        assert!(from.is_some() && to.is_some(), "2024年はうるう年");

        assert!(parse("added:2026-02-29").is_err(), "暦上存在しない日");
        assert!(parse("added:2026-13-01").is_err());
        assert!(parse("added:..").is_err(), "両端省略は不可");
        assert!(parse("added:2026/01/01").is_err());
        assert!(parse("added:>=2026-01-01").is_err(), "範囲指定のみ");
    }

    #[test]
    fn iso_date_bounds() {
        let date = IsoDate {
            year: 2026,
            month: 12,
            day: 31,
        };
        assert_eq!(date.lower_bound_rfc3339(), "2026-12-31T00:00:00.000");
        assert_eq!(
            date.next_day_lower_bound_rfc3339(),
            "2027-01-01T00:00:00.000"
        );
        let date = IsoDate {
            year: 2026,
            month: 1,
            day: 31,
        };
        assert_eq!(
            date.next_day_lower_bound_rfc3339(),
            "2026-02-01T00:00:00.000"
        );
        let date = IsoDate {
            year: 2024,
            month: 2,
            day: 28,
        };
        assert_eq!(
            date.next_day_lower_bound_rfc3339(),
            "2024-02-29T00:00:00.000"
        );
        assert!(!IsoDate::valid(2023, 2, 29));
        assert!(IsoDate::valid(2024, 2, 29));
        assert!(IsoDate::valid(2000, 2, 29));
        assert!(!IsoDate::valid(1900, 2, 29));
    }

    #[test]
    fn query_and_composition() {
        let mut base = parse_ok("夜景 tag:sky").query;
        let gui = parse_ok("rating:>=4 fav:true").query;
        base.and(&gui);
        assert_eq!(base.terms.len(), 4);
        assert_eq!(base.text_terms(), vec!["夜景"]);
        assert_eq!(base.field_conditions().len(), 3);
    }

    // ===== TextPlan (trigram / LIKE割り振り) =====

    #[test]
    fn text_plan_splits_trigram_and_like() {
        let terms = vec!["sunset", "青い髪", "ab", "x"];
        let plan = TextPlan::compile(&terms);
        assert_eq!(plan.match_expr.as_deref(), Some("\"sunset\" \"青い髪\""));
        assert_eq!(plan.like_terms, vec!["ab", "x"]);
    }

    #[test]
    fn text_plan_phrase_with_short_token_falls_back_to_like() {
        let plan = TextPlan::compile(&["ab cd"]);
        assert!(plan.match_expr.is_none());
        assert_eq!(plan.like_terms, vec!["ab cd"]);
        // 短トークン混在句もLIKEへ (phrase MATCHは全トークン>=3文字必要)
        let plan = TextPlan::compile(&["abc de"]);
        assert!(plan.match_expr.is_none());
        assert_eq!(plan.like_terms, vec!["abc de"]);
        let plan = TextPlan::compile(&["abc def"]);
        assert_eq!(plan.match_expr.as_deref(), Some("\"abc def\""));
        assert!(plan.like_terms.is_empty());
    }

    #[test]
    fn text_plan_escapes_quotes_in_match() {
        let plan = TextPlan::compile(&["a\"b\"cde"]);
        assert_eq!(plan.match_expr.as_deref(), Some("\"a\"\"b\"\"cde\""));
    }

    #[test]
    fn text_plan_empty() {
        let plan = TextPlan::compile(&[]);
        assert!(plan.is_empty());
    }

    #[test]
    fn like_escaping() {
        assert_eq!(escape_like("100%"), "100\\%");
        assert_eq!(escape_like("a_b"), "a\\_b");
        assert_eq!(escape_like("x\\y"), "x\\\\y");
        assert_eq!(escape_like("夜景色"), "夜景色");
    }

    // ===== AST JSON (構造の直列化) =====

    #[test]
    fn ast_json_round_trip() {
        let outcome = parse_ok(
            "青い髪の少女 tag:night rating:>=3 type:png added:2026-01-01..2026-06-30 fav:true",
        );
        let json = serde_json::to_string(&outcome.query).unwrap();
        let back: SearchQuery = serde_json::from_str(&json).unwrap();
        assert_eq!(back, outcome.query);
    }
}
