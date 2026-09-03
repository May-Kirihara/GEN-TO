//! 埋め込み画像メタデータ (EXIF / IPTC / PNG生成メタデータ) の解析結果型と
//! repository抽象 (MVP WP-9)。
//!
//! # 2層構造 (data-model §6・spec-draft §12.1)
//!
//! 1. **正規化層** ([`MetadataColumns`]): 検索・表示頻度の高い項目を
//!    `asset_metadata` の既存列へ格納する。Seedは表現差と数値範囲のため
//!    文字列で保持する。
//! 2. **生データ層** (`raw_exif` / `raw_iptc` / `raw_png_metadata` /
//!    `raw_workflow`): 未知項目も破棄せずJSON文字列で保持・表示・コピー
//!    できる (仕様§12.3「未知形式もRaw metadataとして表示」)。
//!
//! # parser行 (asset_metadata 1行 = 1 parserの解析結果)
//!
//! `UNIQUE (asset_id, parser_name)` のため、1 assetに複数parserの行が
//! 縦持ちになる (例: `png-text@1` + `sd-params@1`)。parser名とversionは
//! `parser_name` / `parser_version` に記録し、parser更新時の再解析
//! ([`AssetMetadataRepo::assets_needing_reparse`]) 判定に使う。
//! 名前・versionの正本はgento-image側 (`metadata::CURRENT_PARSERS`)。
//!
//! # 書き込み経路の世代規律 (WP-2パターン踏襲)
//!
//! 解析はハッシュと同じく**1つの開いたfd**に束ねられる
//! ([`crate::library::ScanFs::hash_and_probe_and_metadata`])。保存は
//! リンクと同じ書き込みクロージャ内でDB行の現在トークンと突合した後
//! に行うため、別世代のメタデータが混入する経路は構造的に存在しない。

use crate::repo::{FileRecord, RepoError};

/// 正規化済みメタデータ列 (NULL = 未知/不存在。data-model §6)。
#[derive(Clone, Default, PartialEq)]
pub struct MetadataColumns {
    /// 撮影日時 (RFC3339 `YYYY-MM-DDTHH:MM:SS`。タイムゾーン情報はEXIFに
    /// 依存し通常は不含のためローカル時刻として保持)。
    pub taken_at: Option<String>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens: Option<String>,
    /// GPS緯度 (十進度。南緯は負)。
    pub gps_latitude: Option<f64>,
    /// GPS経度 (十進度。西経は負)。
    pub gps_longitude: Option<f64>,
    /// EXIF Orientation (1..=8)。
    pub orientation: Option<i64>,
    /// 生成ツール (例: "Stable Diffusion WebUI" / "ComfyUI" / "NovelAI")。
    pub generator: Option<String>,
    pub generation_model: Option<String>,
    pub vae: Option<String>,
    pub sampler: Option<String>,
    pub scheduler: Option<String>,
    pub steps: Option<i64>,
    pub cfg_scale: Option<f64>,
    /// 生成ツールによる表現差と数値範囲のため文字列 (data-model §6)。
    pub seed: Option<String>,
    pub positive_prompt: Option<String>,
    pub negative_prompt: Option<String>,
    /// LoRA・サイズ・Clip skip等の追加生成情報 (JSON文字列)。
    pub generation_extra: Option<String>,
}

/// 1 parserの解析結果 (`asset_metadata` 1行分)。
#[derive(Clone, Default, PartialEq)]
pub struct AssetMetadataParsed {
    /// parser名 (例: "exif", "png-text", "sd-params", "comfy", "nai")。
    pub parser_name: String,
    /// parser version (例: "1")。再解析判定の正本。
    pub parser_version: String,
    /// 正規化列 (当該parserが担う列のみ設定)。
    pub columns: MetadataColumns,
    /// 生データJSON (当該parserが担うスロットのみ設定)。
    pub raw_exif: Option<String>,
    pub raw_iptc: Option<String>,
    pub raw_png_metadata: Option<String>,
    pub raw_workflow: Option<String>,
}

/// GPS列を伏せたDebug (WP-15)。GPS座標は個人情報のため、構造体が
/// `{:?}` でログ・エラー文字列へ混入した場合にも値を出さない
/// (「APIキーとGPSがログへ出ない」の構造的防護。値が必要な呼び出し側は
/// フィールドを明示的に読む)。
impl std::fmt::Debug for MetadataColumns {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let gps = if self.gps_latitude.is_some() || self.gps_longitude.is_some() {
            "Some(<gps>)"
        } else {
            "None"
        };
        f.debug_struct("MetadataColumns")
            .field("taken_at", &self.taken_at)
            .field("camera_make", &self.camera_make)
            .field("camera_model", &self.camera_model)
            .field("lens", &self.lens)
            .field("gps_latitude", &gps)
            .field("gps_longitude", &self.gps_longitude.map(|_| "<gps>"))
            .field("orientation", &self.orientation)
            .field("generator", &self.generator)
            .field("generation_model", &self.generation_model)
            .field("vae", &self.vae)
            .field("sampler", &self.sampler)
            .field("scheduler", &self.scheduler)
            .field("steps", &self.steps)
            .field("cfg_scale", &self.cfg_scale)
            .field("seed", &self.seed)
            .field("positive_prompt", &self.positive_prompt)
            .field("negative_prompt", &self.negative_prompt)
            .field("generation_extra", &self.generation_extra)
            .finish()
    }
}

/// rawスロットを要約表示するDebug (WP-15)。raw_exif/raw_iptc等の生JSONは
/// GPS座標 (DMS形式) を含み得るため、バイト数要約のみ出す。
impl std::fmt::Debug for AssetMetadataParsed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let summarize = |raw: &Option<String>| match raw {
            Some(s) => format!("Some(<{} bytes>)", s.len()),
            None => "None".to_string(),
        };
        f.debug_struct("AssetMetadataParsed")
            .field("parser_name", &self.parser_name)
            .field("parser_version", &self.parser_version)
            .field("columns", &self.columns)
            .field("raw_exif", &summarize(&self.raw_exif))
            .field("raw_iptc", &summarize(&self.raw_iptc))
            .field("raw_png_metadata", &summarize(&self.raw_png_metadata))
            .field("raw_workflow", &summarize(&self.raw_workflow))
            .finish()
    }
}

impl MetadataColumns {
    /// 正規化列に1つでも値があるか。
    pub fn has_any_column(&self) -> bool {
        self.taken_at.is_some()
            || self.camera_make.is_some()
            || self.camera_model.is_some()
            || self.lens.is_some()
            || self.gps_latitude.is_some()
            || self.gps_longitude.is_some()
            || self.orientation.is_some()
            || self.generator.is_some()
            || self.generation_model.is_some()
            || self.vae.is_some()
            || self.sampler.is_some()
            || self.scheduler.is_some()
            || self.steps.is_some()
            || self.cfg_scale.is_some()
            || self.seed.is_some()
            || self.positive_prompt.is_some()
            || self.negative_prompt.is_some()
            || self.generation_extra.is_some()
    }
}

impl AssetMetadataParsed {
    /// 正規化列・rawのいずれにも内容がない (保存する価値がない) 場合。
    /// 例: EXIF不在のJPEGに対するexif parserの空結果。
    pub fn is_empty_result(&self) -> bool {
        self.raw_exif.is_none()
            && self.raw_iptc.is_none()
            && self.raw_png_metadata.is_none()
            && self.raw_workflow.is_none()
            && !self.columns.has_any_column()
    }
}

/// `asset_metadata` 読み取り行。
#[derive(Debug, Clone, PartialEq)]
pub struct AssetMetadataRecord {
    pub id: i64,
    pub uuid: String,
    pub asset_id: i64,
    pub parsed: AssetMetadataParsed,
    pub created_at: String,
    pub updated_at: String,
}

/// 再解析候補 (古い/欠落のparser行を持つassetとその代表presentファイル)。
#[derive(Debug, Clone, PartialEq)]
pub struct ReparseTarget {
    pub asset_id: i64,
    pub root_id: i64,
    pub file: FileRecord,
}

/// 埋め込みメタデータのrepository (実装: gento-db)。
pub trait AssetMetadataRepo {
    /// 解析結果を保存する。同parser名の既存行はversionが同じなら置換
    /// しない (内容同一の冪等スキップ)、異なれば置換する。
    /// positive_prompt (呼び出し側の優先順位で選択済み) が空でなければ
    /// `embedded_prompt` キャプション (source=embedded_metadata) を
    /// 同一トランザクションで書き、検索文書を再構築する。
    /// 手動編集済みの現行embedded_prompt行は上書きしない (仕様§10保護)。
    /// 戻り値は行を書き込んだ (または置換した) 場合にtrue。
    fn save_asset_metadata(
        &self,
        asset_id: i64,
        parsed: &[AssetMetadataParsed],
    ) -> Result<bool, RepoError>;

    /// assetの全parser行 (parser名昇順)。
    fn list_asset_metadata(&self, asset_id: i64) -> Result<Vec<AssetMetadataRecord>, RepoError>;

    /// 現行parser集合 `current` (名前→version) に対し再解析が必要なassetを
    /// 列挙する: (a) 行が1つもないasset、(b) 既存行のversionが現行と異なる
    /// asset。形式ごとに生成されるparser行は異なるため「全parser行の存在」は
    /// 要求しない (parser集合の変更は全parserのversion繰り上げで全assetを
    /// 対象化する — gento-image `CURRENT_PARSERS` のdoc参照)。
    /// 代表としてpresentなFileを1件添える (再解析のfd対象)。
    fn assets_needing_reparse(
        &self,
        current: &[(String, String)],
        limit: u64,
    ) -> Result<Vec<ReparseTarget>, RepoError>;
}
