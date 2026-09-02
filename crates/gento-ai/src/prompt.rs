//! versionedプロンプトと応答パース・送信プライバシー制御 (MVP WP-10)。
//!
//! - `PROMPT_VERSION` を bumpedした場合はプロンプト本文変更と
//!   セットでのみ行う (ai_runs.prompt_versionで再生成判定)。
//! - 送信プライバシー (spec §16): EXIF撮影情報とGPS座標は
//!   **登録ルート設定 (library_roots.send_exif / send_gps) と
//!   プロファイル追加設定 (extra_json) のAND** でのみプロンプトへ
//!   含める。GPSはsend_exifとは独立したopt-in。
//! - プロンプトは画像を添え、`{"tags": [...], "caption": "..."}`
//!   形式のJSONのみを要求する (構造化出力非対応endpointでも
//!   動くよう、応答からJSON部分を寛容に抽出する)。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::client::{image_url_message, text_message, ChatMessage};
use crate::imaging::{MAX_IMAGE_EDGE, MIN_IMAGE_EDGE};

/// プロンプトの版。本文を変えたら必ず上げる (ai_runsに記録)。
pub const PROMPT_VERSION: i64 = 1;

const SYSTEM_PROMPT: &str = "\
あなたは画像ライブラリの整理支援アシスタントです。\
指示された形式のJSONだけを応答してください。\
創作ではなく、画像から読み取れる内容にもとづいて答えます。";

const USER_PROMPT_HEADER: &str = "\
添付画像を確認し、次のJSONオブジェクトだけを返してください:
{\"tags\": [\"タグ1\", \"タグ2\", ...], \"caption\": \"一行説明\"}

要件:
- tags: 画像の主要な被写体・構図・雰囲気を表す日本語または英語のタグ。\
8〜15個。固有名詞の推測はしない。
- caption: 画像の内容を1文で説明する日本語。
- JSON以外の説明文・コードブロック装飾は出さない。";

/// AIに渡す撮影情報 (プライバシー制御の透過用。GPSは独立opt-in)。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AiMetadataHints {
    pub taken_at: Option<String>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens: Option<String>,
    pub gps_latitude: Option<f64>,
    pub gps_longitude: Option<f64>,
}

/// プロファイル追加設定 (ai_profiles.extra_json)。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "snake_case")]
pub struct ProfileExtra {
    /// EXIF撮影情報をプロンプトへ含める (ルート設定とのAND)。
    pub send_exif: bool,
    /// GPS座標をプロンプトへ含める (ルート設定とのAND。send_exifとは独立)。
    pub send_gps: bool,
    /// 送信画像の最大辺 (px)。64〜2048の範囲にクランプされる
    /// (0 = 縮小なしは廃止 — 原寸の外部送信を構造的に防ぐ)。
    pub max_image_edge: u32,
    /// response_format: json_object を送る (対応確認済みのendpointのみ)。
    pub send_response_format: bool,
}

impl Default for ProfileExtra {
    fn default() -> Self {
        ProfileExtra {
            send_exif: false,
            send_gps: false,
            max_image_edge: 1024,
            send_response_format: false,
        }
    }
}

impl ProfileExtra {
    /// extra_json文字列をパース (NULL・不正JSONは既定値)。
    /// `max_image_edge` は保存済み値の範囲外 (旧「0=縮小なし」・過大値) を
    /// 読み込み時に [MIN_IMAGE_EDGE, MAX_IMAGE_EDGE] へクランプする
    /// (IPCでの保存は範囲検査で拒否 — 二重の防御)。
    pub fn parse(extra_json: Option<&str>) -> Self {
        let mut parsed = match extra_json {
            Some(text) => serde_json::from_str(text).unwrap_or_default(),
            None => ProfileExtra::default(),
        };
        parsed.max_image_edge = parsed.max_image_edge.clamp(MIN_IMAGE_EDGE, MAX_IMAGE_EDGE);
        parsed
    }

    pub fn to_json_string(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }
}

/// 実効送信設定 (ルート設定 ∧ プロファイル設定)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectivePrivacy {
    pub send_exif: bool,
    pub send_gps: bool,
}

/// ルート設定とプロファイル設定のANDを取る (spec §16)。
pub fn effective_privacy(
    root_send_exif: bool,
    root_send_gps: bool,
    profile: &ProfileExtra,
) -> EffectivePrivacy {
    EffectivePrivacy {
        send_exif: root_send_exif && profile.send_exif,
        send_gps: root_send_gps && profile.send_gps,
    }
}

/// 撮影情報のテキスト節。EXIF節 (send_exif) とGPS座標 (send_gps) は
/// それぞれ独立したopt-in — EXIF拒否でもGPS許可なら座標のみ含める。
fn exif_section(hints: &AiMetadataHints, privacy: EffectivePrivacy) -> String {
    let mut lines: Vec<String> = Vec::new();
    if privacy.send_exif {
        if let Some(taken) = &hints.taken_at {
            lines.push(format!("- 撮影日時: {taken}"));
        }
        let camera = match (&hints.camera_make, &hints.camera_model) {
            (Some(make), Some(model)) => Some(format!("{make} {model}")),
            (Some(make), None) => Some(make.clone()),
            (None, Some(model)) => Some(model.clone()),
            (None, None) => None,
        };
        if let Some(camera) = camera {
            lines.push(format!("- カメラ: {camera}"));
        }
        if let Some(lens) = &hints.lens {
            lines.push(format!("- レンズ: {lens}"));
        }
    }
    // GPSはEXIF節とは独立したopt-in (座標は機密性が高いため)
    if privacy.send_gps {
        if let (Some(lat), Some(lon)) = (hints.gps_latitude, hints.gps_longitude) {
            lines.push(format!("- 撮影位置 (GPS): 緯度 {lat:.6}, 経度 {lon:.6}"));
        }
    }
    if lines.is_empty() {
        return String::new();
    }
    format!("\n\n参考情報 (撮影メタデータ):\n{}", lines.join("\n"))
}

/// タグ+キャプション用のmessagesを構築する。
/// `image_data_url` が None の場合はテキストのみ (応答品質は落ちるが
/// 代表ファイル不在でもジョブを失敗させない設計は取らない —
/// 呼び出し側で必ず代表ファイルを解決してから呼ぶこと)。
pub fn build_caption_tags_messages(
    image_data_url: &str,
    hints: &AiMetadataHints,
    privacy: EffectivePrivacy,
) -> Vec<ChatMessage> {
    let user_text = format!("{USER_PROMPT_HEADER}{}", exif_section(hints, privacy));
    vec![
        ChatMessage::System {
            content: SYSTEM_PROMPT.to_string(),
        },
        image_url_message(image_data_url.to_string(), &user_text),
    ]
}

/// テキストのみのmessages (画像なしのフォールバック用には使わない。
/// プロンプト一貫性の検証で使用)。
pub fn build_caption_tags_text_only(
    hints: &AiMetadataHints,
    privacy: EffectivePrivacy,
) -> Vec<ChatMessage> {
    let user_text = format!(
        "(画像は利用できません) {USER_PROMPT_HEADER}{}",
        exif_section(hints, privacy)
    );
    vec![
        ChatMessage::System {
            content: SYSTEM_PROMPT.to_string(),
        },
        text_message(&user_text),
    ]
}

/// AIの応答から {tags, caption} を取り出す。
///
/// - 純粋JSONを期待するが、コードブロック ```json ... ``` や
///   前後の説明文を寛容に許容する (構造化出力非対応endpoint向け)。
/// - tagsは文字列配列・captionは文字列。欠損フィールドは
///   空扱い (≠エラー: 部分的成功を活かす)。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CaptionTagsResult {
    pub tags: Vec<String>,
    pub caption: Option<String>,
}

pub fn parse_caption_tags_response(content: &str) -> Result<CaptionTagsResult, String> {
    let extracted = extract_json_object(content).ok_or_else(|| {
        format!(
            "応答にJSONオブジェクトが含まれません: {}",
            excerpt_of(content)
        )
    })?;
    let value: Value = serde_json::from_str(extracted)
        .map_err(|e| format!("JSONとして解釈できません ({e}): {}", excerpt_of(extracted)))?;
    let tags = value
        .get("tags")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let caption = value
        .get("caption")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    Ok(CaptionTagsResult { tags, caption })
}

/// 文字列から最初の `{` と対応する最後の `}` で挟まれた部分を取り出す
/// (コードブロック・前置文を除去。ネストは考慮しない — JSON本体の
/// 最初と最後のブレースで十分)。
fn extract_json_object(content: &str) -> Option<&str> {
    let start = content.find('{')?;
    let end = content.rfind('}')?;
    if end <= start {
        return None;
    }
    Some(&content[start..=end])
}

fn excerpt_of(text: &str) -> String {
    text.chars().take(120).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hints() -> AiMetadataHints {
        AiMetadataHints {
            taken_at: Some("2026-08-01T10:00:00Z".to_string()),
            camera_make: Some("TestCam".to_string()),
            camera_model: Some("X-100".to_string()),
            lens: None,
            gps_latitude: Some(35.681236),
            gps_longitude: Some(139.767125),
        }
    }

    #[test]
    fn privacy_is_and_of_root_and_profile() {
        let exif_on = ProfileExtra {
            send_exif: true,
            send_gps: true,
            ..ProfileExtra::default()
        };
        // ルートOFF × プロファイルON → 送らない (ルート単位優先の拒否)
        assert!(!effective_privacy(false, false, &exif_on).send_exif);
        // GPSはsend_exifと独立: ルートGPS=ON × プロファイルGPS=ONなら
        // EXIFがOFFでも座標は送られる (独立opt-in)
        assert!(effective_privacy(false, true, &exif_on).send_gps);
        // ルートGPS=OFFならプロファイルONでも送らない
        assert!(!effective_privacy(true, false, &exif_on).send_gps);
        // ルートON × プロファイルOFF → 送らない
        assert!(!effective_privacy(true, true, &ProfileExtra::default()).send_exif);
        // 両方ON → 送る
        let both = effective_privacy(true, true, &exif_on);
        assert!(both.send_exif && both.send_gps);
    }

    #[test]
    fn gps_is_omitted_unless_both_gates_on() {
        let h = hints();
        let profile_both = ProfileExtra {
            send_exif: true,
            send_gps: true,
            ..ProfileExtra::default()
        };
        // send_exifのみON (GPS拒否): EXIF節はあるが座標は無い
        let exif_only = effective_privacy(true, false, &profile_both);
        let messages = build_caption_tags_messages("data:image/jpeg;base64,xx", &h, exif_only);
        let json = serde_json::to_string(&messages).unwrap();
        assert!(json.contains("TestCam"), "EXIFは含まれる: {json}");
        assert!(!json.contains("35.68"), "GPS座標は含まれない: {json}");
        assert!(!json.contains("139.76"));

        // 両方ON: 座標が含まれる
        let both = effective_privacy(true, true, &profile_both);
        let messages = build_caption_tags_messages("data:image/jpeg;base64,xx", &h, both);
        let json = serde_json::to_string(&messages).unwrap();
        assert!(json.contains("35.681236"), "GPS座標が含まれる: {json}");

        // GPSはEXIFと独立: EXIF拒否・GPS許可なら座標のみ含まれる
        let gps_only = effective_privacy(false, true, &profile_both);
        let messages = build_caption_tags_messages("data:image/jpeg;base64,xx", &h, gps_only);
        let json = serde_json::to_string(&messages).unwrap();
        assert!(
            json.contains("35.681236"),
            "EXIF拒否でもGPS許可なら座標は含まれる: {json}"
        );
        assert!(!json.contains("TestCam"), "EXIF機種名は含まれない: {json}");
        assert!(
            !json.contains("50mm") && !json.contains("2026-08-01"),
            "EXIF節なし: {json}"
        );

        // 両方拒否: 何も含まれない
        let none = effective_privacy(false, false, &profile_both);
        let messages = build_caption_tags_messages("data:image/jpeg;base64,xx", &h, none);
        let json = serde_json::to_string(&messages).unwrap();
        assert!(!json.contains("TestCam"));
        assert!(!json.contains("35.68"));
        assert!(!json.contains("撮影メタデータ"));
    }

    #[test]
    fn prompt_asks_json_tags_caption() {
        let messages = build_caption_tags_messages(
            "data:image/jpeg;base64,xx",
            &AiMetadataHints::default(),
            EffectivePrivacy {
                send_exif: false,
                send_gps: false,
            },
        );
        let json = serde_json::to_string(&messages).unwrap();
        assert!(json.contains("tags"));
        assert!(json.contains("caption"));
        assert!(json.contains("image_url"));
        assert!(json.contains("data:image/jpeg;base64,xx"));
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn parse_accepts_pure_json_codeblock_and_prose() {
        let pure =
            parse_caption_tags_response(r#"{"tags":["空","雲"],"caption":"青空と雲"}"#).unwrap();
        assert_eq!(pure.tags, vec!["空", "雲"]);
        assert_eq!(pure.caption.as_deref(), Some("青空と雲"));

        let fenced = parse_caption_tags_response(
            "以下が結果です:\n```json\n{\"tags\":[\"cat\"],\"caption\":\"a cat\"}\n```\n以上。",
        )
        .unwrap();
        assert_eq!(fenced.tags, vec!["cat"]);

        let empty_tags = parse_caption_tags_response(r#"{"tags":[],"caption":""}"#).unwrap();
        assert!(empty_tags.tags.is_empty());
        assert!(empty_tags.caption.is_none(), "空captionはNone扱い");
    }

    #[test]
    fn parse_rejects_non_json() {
        assert!(parse_caption_tags_response("ただの文章です").is_err());
        assert!(parse_caption_tags_response("").is_err());
    }

    #[test]
    fn profile_extra_defaults_and_roundtrip() {
        let default = ProfileExtra::default();
        assert!(!default.send_exif);
        assert!(!default.send_gps);
        assert_eq!(default.max_image_edge, 1024);
        assert!(!default.send_response_format);
        // NULL・不正JSONは既定値
        assert_eq!(ProfileExtra::parse(None), default);
        assert_eq!(ProfileExtra::parse(Some("{bogus")), default);
        let parsed = ProfileExtra::parse(Some(r#"{"send_exif":true,"max_image_edge":512}"#));
        assert!(parsed.send_exif);
        assert_eq!(parsed.max_image_edge, 512);
        assert!(!parsed.send_gps);
        // 往復
        let s = parsed.to_json_string();
        assert_eq!(ProfileExtra::parse(Some(&s)), parsed);
        // 保存済み値の範囲外 (旧「0=縮小なし」・過大値) は読み込み時に
        // クランプされる (IPC保存側は拒否 — tauri ai.rsのテスト参照)
        assert_eq!(
            ProfileExtra::parse(Some(r#"{"max_image_edge":0}"#)).max_image_edge,
            MIN_IMAGE_EDGE
        );
        assert_eq!(
            ProfileExtra::parse(Some(r#"{"max_image_edge":8192}"#)).max_image_edge,
            MAX_IMAGE_EDGE
        );
        assert_eq!(
            ProfileExtra::parse(Some(r#"{"max_image_edge":100}"#)).max_image_edge,
            100,
            "範囲内は不変"
        );
    }

    #[test]
    fn prompt_version_is_positive() {
        // 型はi64 (ai_runs.prompt_version CHECK >= 1との整合)
        let v: i64 = PROMPT_VERSION;
        assert!(v >= 1);
    }
}
