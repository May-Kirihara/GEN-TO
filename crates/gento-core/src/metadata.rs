//! アセットメタデータ (タグ・キャプション) のrepository抽象 (MVP WP-5)。
//!
//! 評価・選別・お気に入りは[`crate::repo::AssetRecord`]の列なので
//! [`crate::repo::AssetRepo`]へsetterを追加する (repo.rs)。本モジュールは
//! タグ (tags / asset_tags) とキャプション (captions) のレコード型・
//! trait・正規化規則を担う。
//!
//! # タグ正規化規則 (MVP実装計画§13「タグ正規化規則を固定する」)
//!
//! [`normalize_tag_name`] は次の順で適用する:
//!
//! 1. 前後のUnicode空白を除去 (trim)
//! 2. NFC正規化 (結合文字列の合成)
//! 3. ASCII範囲の大文字→小文字
//! 4. 内部の空白連続を1つの半角空白へ圧縮
//!
//! 仕様上の帰結:
//! - 全角英数字 (Ａ等) はNFKCを適用しないため別タグのまま (ASCII小文字化のみ)
//! - 全角空白 (U+3000)・タブ等も「空白」として扱われ半角空白1つへ畳まる
//! - 正規化結果が空文字列になる入力はタグ名として不成立
//!   ([`RepoError::Constraint`])
//! - `tags.normalized_name` はUNIQUEのため、表記違いの同一タグは
//!   作成時に既存タグへ統合される (重複→統合)
//!
//! # キャプションの現行/履歴 (data-model §5)
//!
//! kind (description / sd_tags / user_note / 将来: ocr・embedded_prompt等)
//! ごとに `current=1` の行は高々1行 (schemaの部分UNIQUE index)。
//! 手動編集は「新行の挿入 + 旧現行行のcurrent=0化」で行い、旧内容を
//! 履歴として残す。手動編集の行は `source='manual'`・`manually_edited=1`
//! となり、WP-10のAI再生成で上書きされない (手動編集済み保護)。
//!
//! WP-10接続に向けて: `ai_run_id`列はai_runsテーブル作成と同時に
//! ADD COLUMNされる (schema.rsの方式コメント参照)。AI由来キャプションは
//! `source='ai'`で挿入される想定であり、本WPは構造のみ用意する。

use std::fmt;

use crate::repo::RepoError;

/// タグ (tagsテーブル) のレコード。`usage_count` はasset_tagsとの結合結果
/// (結合なしで読んだ場合は0)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagRecord {
    pub id: i64,
    pub uuid: String,
    /// 表示名 (作成時の入力から前後空白のみ除去したもの。表記は初回登録時のまま)。
    pub display_name: String,
    /// 正規化名 (UNIQUE。[`normalize_tag_name`]の出力)。
    pub normalized_name: String,
    pub category: Option<String>,
    /// このタグを付与されたAsset数 (LEFT JOIN集計。無付与は0)。
    pub usage_count: u64,
    pub created_at: String,
    pub updated_at: String,
}

/// タグ付与 (asset_tags) の由来 (data-model §4.2)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagSource {
    Manual,
    Ai,
    Import,
    EmbeddedMetadata,
}

impl TagSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            TagSource::Manual => "manual",
            TagSource::Ai => "ai",
            TagSource::Import => "import",
            TagSource::EmbeddedMetadata => "embedded_metadata",
        }
    }
}

impl fmt::Display for TagSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Assetへのタグ付与 (asset_tags) のレコード (タグ本体を同梱)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetTagRecord {
    pub asset_id: i64,
    pub tag: TagRecord,
    pub source: TagSource,
    /// 手動編集済み (AI再生成時の保護フラグ。手動追加・手動再付与で1)。
    pub manually_edited: bool,
    pub created_at: String,
}

/// キャプション種別 (captions.kind。data-model §5)。
/// MVP編集UIが対象にするのは Description / SdTags / UserNote の3種。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptionKind {
    Description,
    SdTags,
    UserNote,
    Ocr,
    EmbeddedPrompt,
    EmbeddedNegativePrompt,
}

impl CaptionKind {
    pub const ALL: [CaptionKind; 6] = [
        CaptionKind::Description,
        CaptionKind::SdTags,
        CaptionKind::UserNote,
        CaptionKind::Ocr,
        CaptionKind::EmbeddedPrompt,
        CaptionKind::EmbeddedNegativePrompt,
    ];

    /// MVP編集UI対象の3種。
    pub const EDITABLE: [CaptionKind; 3] = [
        CaptionKind::Description,
        CaptionKind::SdTags,
        CaptionKind::UserNote,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            CaptionKind::Description => "description",
            CaptionKind::SdTags => "sd_tags",
            CaptionKind::UserNote => "user_note",
            CaptionKind::Ocr => "ocr",
            CaptionKind::EmbeddedPrompt => "embedded_prompt",
            CaptionKind::EmbeddedNegativePrompt => "embedded_negative_prompt",
        }
    }
}

impl fmt::Display for CaptionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// キャプション由来 (captions.source。data-model §5)。
/// AIはWP-10で接続される (本WPでは構造のみ)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptionSource {
    Manual,
    Ai,
    Import,
    EmbeddedMetadata,
}

impl CaptionSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            CaptionSource::Manual => "manual",
            CaptionSource::Ai => "ai",
            CaptionSource::Import => "import",
            CaptionSource::EmbeddedMetadata => "embedded_metadata",
        }
    }
}

impl fmt::Display for CaptionSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// キャプション (captions) のレコード。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptionRecord {
    pub id: i64,
    pub uuid: String,
    pub asset_id: i64,
    pub kind: CaptionKind,
    pub body: String,
    pub source: CaptionSource,
    pub language: Option<String>,
    /// 現行 (kindごとに高々1行)。
    pub is_current: bool,
    /// 手動編集済み (AI再生成で上書きしない。仕様§10)。
    pub manually_edited: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// タグのrepository。
///
/// 削除は「使用中のasset_tagsごと削除」で実行する (付随解除。
/// tags削除に対しasset_tagsはON DELETE CASCADE)。タグだけを残して
/// 付与を外す操作はAssetTagRepo側の担当。
pub trait TagRepo {
    /// タグを作成する。入力は[`normalize_tag_name`]で正規化し、
    /// 空になった場合は[`RepoError::Constraint`]。`normalized_name`が
    /// 既存タグと衝突する場合は新規作成せず**既存タグを返す**
    /// (表記違いの統合)。
    fn create_tag(&self, name: &str) -> Result<TagRecord, RepoError>;

    /// 正規化名でタグを引く (`normalize_tag_name`を適用して照合)。
    fn find_tag_by_name(&self, name: &str) -> Result<Option<TagRecord>, RepoError>;

    /// 主キーでタグを引く。
    fn get_tag(&self, tag_id: i64) -> Result<Option<TagRecord>, RepoError>;

    /// 候補表示: 正規化名の前方一致・使用数降順 (同数は正規化名昇順)。
    /// `prefix`も正規化して照合する。空prefixは全体の一覧と同義。
    fn suggest_tags(&self, prefix: &str, limit: u32) -> Result<Vec<TagRecord>, RepoError>;

    /// 全タグ一覧 (使用数降順・正規化名昇順)。
    fn list_tags(&self) -> Result<Vec<TagRecord>, RepoError>;

    /// タグを削除する (付与されているasset_tagsもCASCADEで削除)。
    /// 存在しないIDは[`RepoError::NotFound`]。
    fn delete_tag(&self, tag_id: i64) -> Result<(), RepoError>;
}

/// Asset↔Tagの付与 (asset_tags) のrepository。
pub trait AssetTagRepo {
    /// 複数Assetへ複数タグ名を一括付与する (手動追加。
    /// source=manual・manually_edited=1)。
    /// タグは正規化して作成 or 既存統合。1トランザクションで実行し、
    /// 一部Asset不在などで失敗した場合は全体をロールバックする
    /// (一部だけ付与された状態を残さない)。
    /// 戻り値は付与後の (asset_id, タグ) 組に関連する付与レコード。
    fn add_tags_batch(
        &self,
        asset_ids: &[i64],
        names: &[&str],
    ) -> Result<Vec<AssetTagRecord>, RepoError>;

    /// 付与を1件解除する。付与が存在しない場合は[`RepoError::NotFound`]。
    fn remove_tag_from_asset(&self, asset_id: i64, tag_id: i64) -> Result<(), RepoError>;

    /// Assetのタグ一覧 (タグ名昇順)。
    fn list_asset_tags(&self, asset_id: i64) -> Result<Vec<AssetTagRecord>, RepoError>;

    /// このタグを付与されているAssetのID一覧 (タグ削除前の影響範囲特定用)。
    fn assets_of_tag(&self, tag_id: i64) -> Result<Vec<i64>, RepoError>;
}

/// キャプション (captions) のrepository。
pub trait CaptionRepo {
    /// 手動編集を確定する (upsert相当の現行入れ替え)。
    /// 現行行があれば`current=0`へ降格 (履歴保持) し、新しい現行行を
    /// `source=manual`・`manually_edited=1`で挿入する。
    /// Asset不在は[`RepoError::NotFound`]。
    fn set_caption(
        &self,
        asset_id: i64,
        kind: CaptionKind,
        body: &str,
    ) -> Result<CaptionRecord, RepoError>;

    /// 現行キャプションを取得する (無ければNone)。
    fn get_current_caption(
        &self,
        asset_id: i64,
        kind: CaptionKind,
    ) -> Result<Option<CaptionRecord>, RepoError>;

    /// 履歴を含む一覧 (現行を先頭に、新しい順)。
    fn list_caption_history(
        &self,
        asset_id: i64,
        kind: CaptionKind,
        limit: u32,
    ) -> Result<Vec<CaptionRecord>, RepoError>;
}

/// タグ名の正規化 (モジュールdocの規則参照)。
pub fn normalize_tag_name(input: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    let trimmed = input.trim();
    let nfc: String = trimmed.nfc().collect();
    let lowered = nfc.to_ascii_lowercase();
    lowered.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_trims_and_lowercases_ascii() {
        assert_eq!(normalize_tag_name("  Sky  "), "sky");
        assert_eq!(normalize_tag_name("  SKY  "), "sky");
        assert_eq!(normalize_tag_name("Blue Sky"), "blue sky");
    }

    #[test]
    fn normalize_collapses_internal_whitespace() {
        // 半角複数・タブ・全角空白 (U+3000) も1つの半角空白へ
        assert_eq!(normalize_tag_name("a  b"), "a b");
        assert_eq!(normalize_tag_name("a\t\tb"), "a b");
        assert_eq!(normalize_tag_name("青　空"), "青 空");
        assert_eq!(normalize_tag_name(" a   b\tc  "), "a b c");
    }

    #[test]
    fn normalize_applies_nfc() {
        // 分解形 (U+304B + U+3099) → 合成形 (U+304C)
        let decomposed = "\u{304B}\u{3099}";
        let composed = "\u{304C}";
        assert_eq!(normalize_tag_name(decomposed), composed);
        // NFC後もASCII小文字化・空白圧縮は作用する
        assert_eq!(normalize_tag_name("  A\u{304B}\u{3099}  B "), "a\u{304C} b",);
    }

    #[test]
    fn normalize_keeps_fullwidth_ascii_distinct() {
        // NFKCは適用しない: 全角Ａと半角Aは別タグ (仕様判断)
        assert_eq!(normalize_tag_name("ＴＡＧ"), "ＴＡＧ");
        assert_ne!(normalize_tag_name("ＴＡＧ"), normalize_tag_name("TAG"));
        // 全角英字はASCII小文字化の対象外 (そのまま)
        assert_eq!(normalize_tag_name("ａｂｃ"), "ａｂｃ");
    }

    #[test]
    fn normalize_empty_and_whitespace_only() {
        assert_eq!(normalize_tag_name(""), "");
        assert_eq!(normalize_tag_name("   "), "");
        assert_eq!(normalize_tag_name("　"), "");
    }

    #[test]
    fn caption_kinds_round_trip_strings() {
        for kind in CaptionKind::ALL {
            assert!(!kind.as_str().is_empty());
            assert!(kind
                .as_str()
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '_'));
        }
        assert_eq!(CaptionKind::Description.as_str(), "description");
        assert_eq!(CaptionKind::SdTags.as_str(), "sd_tags");
        assert_eq!(CaptionKind::UserNote.as_str(), "user_note");
        // 編集UI対象は3種のみ
        assert_eq!(CaptionKind::EDITABLE.len(), 3);
    }

    #[test]
    fn tag_sources_str_values() {
        assert_eq!(TagSource::Manual.as_str(), "manual");
        assert_eq!(TagSource::Ai.as_str(), "ai");
        assert_eq!(TagSource::Import.as_str(), "import");
        assert_eq!(TagSource::EmbeddedMetadata.as_str(), "embedded_metadata");
        assert_eq!(CaptionSource::Manual.as_str(), "manual");
        assert_eq!(CaptionSource::Ai.as_str(), "ai");
    }
}
