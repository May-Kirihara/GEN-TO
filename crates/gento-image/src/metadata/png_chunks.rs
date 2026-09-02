//! PNGテキストチャンク (tEXt / iTXt / zTXt) とeXIfチャンクの収集 (WP-9)。
//!
//! 自前のバイト走査 (png crateの全文デコードを使わない — IDAT等の
//! 巨大チャンクはseekで読み飛ばし、テキストチャンクのみを上限付きで
//! 読む)。上限: 1チャンク `PNG_CHUNK_LIMIT_BYTES` (4MiB)・テキスト合計
//! `PNG_TEXT_TOTAL_LIMIT_BYTES` (16MiB)。超過チャンクは警告付きスキップ
//! (巨大workflowで過大メモリ確保・UI停止を防ぐ。警告はraw JSONの
//! `__warnings` へ記録されUIで見える)。

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};

use gento_core::assetmeta::{AssetMetadataParsed, MetadataColumns};

use super::PARSER_PNG_TEXT;

/// 1テキストチャンクの読み取り上限 (バイト)。
pub const PNG_CHUNK_LIMIT_BYTES: usize = 4 * 1024 * 1024;
/// テキストチャンク合計の読み取り上限 (バイト)。
pub const PNG_TEXT_TOTAL_LIMIT_BYTES: usize = 16 * 1024 * 1024;
/// 走査するチャンク数の上限 (異常ファイルへの安全弁)。
const MAX_CHUNKS: usize = 8192;

/// PNG走査結果。
pub(crate) struct PngTextChunks {
    /// テキストチャンク (keyword, text)。ファイル順。
    pub texts: Vec<(String, String)>,
    /// eXIfチャンクのTIFF本体 (`Exif\0\0` ヘッダ付きの可能性は読み手側で処理)。
    pub exif: Option<Vec<u8>>,
    /// 上限超過等の警告 (raw JSONへ記録)。
    pub warnings: Vec<String>,
}

/// PNGシグネチャ。
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// PNGテキストチャンク・eXIfを収集する。PNGでない・読み取れない・
/// 収集対象が1つもない場合はNone。
pub(crate) fn collect_png_texts(file: &mut File, cancel: &AtomicBool) -> Option<PngTextChunks> {
    if file.rewind().is_err() {
        return None;
    }
    let mut sig = [0u8; 8];
    file.read_exact(&mut sig).ok()?;
    if sig != PNG_SIGNATURE {
        return None;
    }
    let mut out = PngTextChunks {
        texts: Vec::new(),
        exif: None,
        warnings: Vec::new(),
    };
    let mut text_total = 0usize;
    let mut header = [0u8; 8]; // len(4) + type(4)
    for _ in 0..MAX_CHUNKS {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        if file.read_exact(&mut header).is_err() {
            break; // 切り詰め — 収集済み分で妥協
        }
        let len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let kind = [header[4], header[5], header[6], header[7]];
        if &kind == b"IEND" {
            break;
        }
        let interesting = matches!(&kind, b"tEXt" | b"zTXt" | b"iTXt" | b"eXIf");
        if !interesting
            || len > PNG_CHUNK_LIMIT_BYTES
            || text_total.saturating_add(len) > PNG_TEXT_TOTAL_LIMIT_BYTES
        {
            let skip = len + 4; // データ + CRC
            if len > PNG_CHUNK_LIMIT_BYTES && interesting {
                out.warnings.push(format!(
                    "チャンク {} ({}バイト) は上限 ({}バイト) 超過のためスキップしました",
                    String::from_utf8_lossy(&kind),
                    len,
                    PNG_CHUNK_LIMIT_BYTES
                ));
            } else if interesting && text_total.saturating_add(len) > PNG_TEXT_TOTAL_LIMIT_BYTES {
                out.warnings.push(format!(
                    "チャンク {} でテキスト合計上限 ({}バイト) に到達したためスキップしました",
                    String::from_utf8_lossy(&kind),
                    PNG_TEXT_TOTAL_LIMIT_BYTES
                ));
            }
            if file.seek(SeekFrom::Current(skip as i64)).is_err() {
                break;
            }
            continue;
        }
        let mut data = vec![0u8; len];
        if file.read_exact(&mut data).is_err() {
            break;
        }
        if file.seek(SeekFrom::Current(4)).is_err() {
            break; // CRC読み飛ばし
        }
        match &kind {
            b"tEXt" => {
                if let Some((keyword, text)) = split_null(&data) {
                    text_total = text_total.saturating_add(len);
                    out.texts
                        .push((latin1_to_string(keyword), latin1_to_string(text)));
                }
            }
            b"zTXt" => {
                if let Some((keyword, rest)) = split_null(&data) {
                    // rest = compression_method(1) + zlib data
                    if rest.first() == Some(&0) {
                        match inflate_limited(&rest[1..], PNG_CHUNK_LIMIT_BYTES) {
                            Some(text) => {
                                text_total = text_total.saturating_add(len);
                                out.texts
                                    .push((latin1_to_string(keyword), latin1_to_string(&text)));
                            }
                            None => out.warnings.push(format!(
                                "チャンク zTXt \"{}\" の解凍に失敗したためスキップしました",
                                latin1_to_string(keyword)
                            )),
                        }
                    } else {
                        out.warnings.push(format!(
                            "チャンク zTXt \"{}\" は未知の圧縮方式のためスキップしました",
                            latin1_to_string(keyword)
                        ));
                    }
                }
            }
            b"iTXt" => {
                if let Some(parsed) = parse_itxt(&data) {
                    text_total = text_total.saturating_add(len);
                    out.texts.push(parsed);
                }
            }
            b"eXIf" => {
                text_total = text_total.saturating_add(len);
                // 本体が "Exif\0\0" ヘッダを含む書き手が存在するため剥がす
                let tiff = data
                    .strip_prefix(b"Exif\0\0")
                    .map(<[u8]>::to_vec)
                    .unwrap_or(data);
                if out.exif.is_none() {
                    out.exif = Some(tiff);
                }
            }
            _ => unreachable!("interesting判定済み"),
        }
    }
    if out.texts.is_empty() && out.exif.is_none() && out.warnings.is_empty() {
        return None;
    }
    Some(out)
}

/// 収集結果を `png-text@1` 行へ組み立てる (raw_png_metadata JSON)。
pub(crate) fn png_text_row(chunks: &PngTextChunks) -> AssetMetadataParsed {
    let mut map = serde_json::Map::new();
    for (keyword, text) in &chunks.texts {
        // 同一keywordの重複チャンクは後勝ち (JSONオブジェクトの制約)。
        map.insert(keyword.clone(), serde_json::Value::String(text.clone()));
    }
    if !chunks.warnings.is_empty() {
        map.insert(
            "__warnings".to_string(),
            serde_json::Value::Array(
                chunks
                    .warnings
                    .iter()
                    .map(|w| serde_json::Value::String(w.clone()))
                    .collect(),
            ),
        );
    }
    AssetMetadataParsed {
        parser_name: PARSER_PNG_TEXT.to_string(),
        parser_version: "1".to_string(),
        columns: MetadataColumns::default(),
        raw_exif: None,
        raw_iptc: None,
        raw_png_metadata: Some(serde_json::to_string(&serde_json::Value::Object(map)).unwrap()),
        raw_workflow: None,
    }
}

/// 最初のNULで分割 (NUL不在・空keywordはNone)。
fn split_null(data: &[u8]) -> Option<(&[u8], &[u8])> {
    let pos = data.iter().position(|&b| b == 0)?;
    if pos == 0 {
        return None;
    }
    Some((&data[..pos], &data[pos + 1..]))
}

/// iTXt: keyword\0 comp_flag comp_method language\0 translated\0 text(UTF-8)。
fn parse_itxt(data: &[u8]) -> Option<(String, String)> {
    let (keyword, rest) = split_null(data)?;
    if keyword.is_empty() {
        return None;
    }
    let (flag, rest) = rest.split_first()?;
    let (_method, rest) = rest.split_first()?;
    let pos = rest.iter().position(|&b| b == 0)?;
    let language = &rest[..pos];
    let rest = &rest[pos + 1..];
    let pos2 = rest.iter().position(|&b| b == 0)?;
    let translated = &rest[..pos2];
    let text = &rest[pos2 + 1..];
    let decoded = if *flag == 1 {
        // 圧縮付き
        inflate_limited(text, PNG_CHUNK_LIMIT_BYTES)?
    } else {
        text.to_vec()
    };
    let _ = (language, translated); // 言語タグはraw保持しない (テキスト本体のみ)
    Some((
        latin1_to_string(keyword),
        String::from_utf8_lossy(&decoded).into_owned(),
    ))
}

/// zlib解凍 (出力上限付き。上限超過・不正ストリームはNone)。
fn inflate_limited(data: &[u8], limit: usize) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let mut decoder = flate2::read::ZlibDecoder::new(data).take((limit + 1) as u64);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).ok()?;
    if out.len() > limit {
        return None; // 解凍bomp — 上限超過
    }
    Some(out)
}

/// Latin-1 (ISO-8859-1) バイト列 → String (1:1でU+00FFまで対応)。
/// tEXt/zTXtの仕様上の文字コード。
pub(crate) fn latin1_to_string(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

#[cfg(test)]
mod tests {
    use super::super::test_support as ts;
    use super::*;

    fn collect(bytes: &[u8]) -> Option<PngTextChunks> {
        // メモリからfd経由へ: 一時ファイルに書いて開く (fd束ね経路の検証)
        let dir = std::env::temp_dir().join(format!(
            "gento-pngtext-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.png");
        std::fs::write(&path, bytes).unwrap();
        let file = File::open(&path).unwrap();
        let out = collect_png_texts(&mut { file }, &AtomicBool::new(false));
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    fn text_chunk(keyword: &str, text: &str) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(keyword.as_bytes());
        data.push(0);
        data.extend_from_slice(text.as_bytes());
        ts::png_chunk(b"tEXt", &data)
    }

    #[test]
    fn collects_text_iext_and_exif_in_order() {
        let mut png = ts::png_header();
        png.extend_from_slice(&text_chunk("Title", "AI generated image"));
        // iTXt非圧縮
        let mut itxt = Vec::new();
        itxt.extend_from_slice(b"Comment");
        itxt.push(0);
        itxt.push(0); // comp_flag = 0
        itxt.push(0); // comp_method
        itxt.push(0); // language終端
        itxt.push(0); // translated終端
        itxt.extend_from_slice("nai prompt text".as_bytes());
        png.extend_from_slice(&ts::png_chunk(b"iTXt", &itxt));
        png.extend_from_slice(&ts::png_chunk(b"eXIf", &ts::tiff_orientation_only(3)));
        png.extend_from_slice(&ts::png_chunk(b"IEND", &[]));

        let chunks = collect(&png).unwrap();
        assert_eq!(
            chunks.texts,
            vec![
                ("Title".to_string(), "AI generated image".to_string()),
                ("Comment".to_string(), "nai prompt text".to_string()),
            ]
        );
        assert!(chunks.exif.is_some());
        assert!(chunks.warnings.is_empty());
    }

    #[test]
    fn ztxt_decompresses_zlib() {
        // zTXt: keyword\0 method(0) + zlib(圧縮された長文)
        let long_text = "very long workflow json ".repeat(200);
        let mut data = Vec::new();
        data.extend_from_slice(b"workflow");
        data.push(0);
        data.push(0);
        data.extend_from_slice(&ts::zlib_stored(long_text.as_bytes()));
        let mut png = ts::png_header();
        png.extend_from_slice(&ts::png_chunk(b"zTXt", &data));
        png.extend_from_slice(&ts::png_chunk(b"IEND", &[]));

        let chunks = collect(&png).unwrap();
        assert_eq!(chunks.texts.len(), 1);
        assert_eq!(chunks.texts[0].0, "workflow");
        assert_eq!(chunks.texts[0].1.len(), long_text.len());
    }

    #[test]
    fn itxt_compressed_decompresses() {
        let mut data = Vec::new();
        data.extend_from_slice(b"prompt");
        data.push(0);
        data.push(1); // comp_flag = 1
        data.push(0); // comp_method = 0 (zlib)
        data.push(0); // language終端
        data.push(0); // translated終端
        data.extend_from_slice(&ts::zlib_stored(b"1girl, sunset"));
        let mut png = ts::png_header();
        png.extend_from_slice(&ts::png_chunk(b"iTXt", &data));
        png.extend_from_slice(&ts::png_chunk(b"IEND", &[]));
        let chunks = collect(&png).unwrap();
        assert_eq!(
            chunks.texts[0],
            ("prompt".to_string(), "1girl, sunset".to_string())
        );
    }

    #[test]
    fn oversize_chunk_is_skipped_with_warning() {
        // 5MiB の tEXt → 1チャンク上限 (4MiB) 超過でスキップ + 警告
        let huge = vec![b'x'; 5 * 1024 * 1024];
        let mut data = Vec::new();
        data.extend_from_slice(b"big");
        data.push(0);
        data.extend_from_slice(&huge);
        let mut png = ts::png_header();
        png.extend_from_slice(&text_chunk("small", "keep me"));
        png.extend_from_slice(&ts::png_chunk(b"tEXt", &data));
        png.extend_from_slice(&ts::png_chunk(b"IEND", &[]));

        let chunks = collect(&png).unwrap();
        assert_eq!(chunks.texts.len(), 1, "上限内チャンクは保持");
        assert_eq!(chunks.texts[0].0, "small");
        assert_eq!(chunks.warnings.len(), 1, "超過チャンクの警告");
        assert!(
            chunks.warnings[0].contains("tEXt"),
            "{}",
            chunks.warnings[0]
        );
        assert!(
            chunks.warnings[0].contains("上限"),
            "{}",
            chunks.warnings[0]
        );

        // raw JSONに警告が記録される
        let row = png_text_row(&chunks);
        let raw =
            serde_json::from_str::<serde_json::Value>(row.raw_png_metadata.as_deref().unwrap())
                .unwrap();
        assert!(raw["small"].is_string());
        assert!(raw["__warnings"].is_array());
    }

    #[test]
    fn total_limit_stops_collection() {
        // 4MiB×5 = 20MiB > 合計16MiB → 後半チャンクは合計上限でスキップ
        let mut png = ts::png_header();
        for i in 0..5 {
            let mut data = Vec::new();
            data.extend_from_slice(format!("k{i}").as_bytes());
            data.push(0);
            data.extend_from_slice(&vec![b'a'; 4 * 1024 * 1024 - 8]);
            png.extend_from_slice(&ts::png_chunk(b"tEXt", &data));
        }
        png.extend_from_slice(&ts::png_chunk(b"IEND", &[]));
        let chunks = collect(&png).unwrap();
        assert!(
            chunks.texts.len() < 5,
            "合計上限で打ち切り: {n}",
            n = chunks.texts.len()
        );
        assert!(chunks.texts.len() >= 3, "上限までのチャンクは保持");
        assert!(!chunks.warnings.is_empty());
    }

    #[test]
    fn plain_png_without_text_returns_none() {
        let png = crate::probe::minimal_png_bytes(4, 4);
        assert!(collect(&png).is_none());
    }

    #[test]
    fn corrupt_png_best_effort() {
        // シグネチャのみで切り詰め — 収集対象なし = None
        let png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        assert!(collect(&png).is_none());
    }

    #[test]
    fn latin1_high_bytes_map_to_unicode() {
        // Latin-1の0xE9 (é) はU+00E9へ写像される (UTF-8バイトとして誤読しない)
        let s = latin1_to_string(&[0xE9, 0xFC]); // é ü
        assert_eq!(s, "\u{00E9}\u{00FC}");
        assert_eq!(s.len(), 4); // UTF-8で2バイト×2
    }
}
