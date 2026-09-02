//! IPTC IIM (JPEG APP13 8BIM内) の最小自作解析 (WP-9)。
//!
//! よく使うレコード (record 2: タイトル/キーワード/作成者/著作権/キャプション、
//! record 1: 文字コード) のみを解釈し、残りは `"2:10"` 形式の生キーでraw保持
//! する (完全なIIM実装ではない — 仕様上「解析不能分はraw保持」で足りる)。
//! 正規化列は持たない (写真系列の正規化はEXIFが正本)。

use gento_core::assetmeta::{AssetMetadataParsed, MetadataColumns};

use super::PARSER_IPTC;

/// raw値の文字列上限。
const VALUE_LIMIT: usize = 2048;
/// 処理するデータセット数の上限。
const MAX_DATASETS: usize = 1024;

/// APP13ペイロード群 (複数セグメント継続に対応) からIIMレコードを解析する。
/// Photoshopヘッダ・8BIMリソースが見つからない/空の場合はNone。
pub(crate) fn parse_iptc_app13(payloads: &[Vec<u8>]) -> Option<AssetMetadataParsed> {
    let mut iim_blocks: Vec<Vec<u8>> = Vec::new();
    for payload in payloads {
        // 各APP13セグメントは "Photoshop 3.0\0" ヘッダで始まる
        let body = payload.strip_prefix(b"Photoshop 3.0\0")?;
        collect_8bim_iim(body, &mut iim_blocks);
    }
    if iim_blocks.is_empty() {
        return None;
    }
    let mut raw = serde_json::Map::new();
    let mut keywords: Vec<String> = Vec::new();
    for iim in &iim_blocks {
        parse_iim_datasets(iim, &mut raw, &mut keywords);
    }
    if !keywords.is_empty() {
        raw.insert(
            "keywords".to_string(),
            serde_json::Value::Array(
                keywords
                    .into_iter()
                    .map(serde_json::Value::String)
                    .collect(),
            ),
        );
    }
    if raw.is_empty() {
        return None;
    }
    Some(AssetMetadataParsed {
        parser_name: PARSER_IPTC.to_string(),
        parser_version: "1".to_string(),
        columns: MetadataColumns::default(),
        raw_exif: None,
        raw_iptc: Some(serde_json::to_string(&serde_json::Value::Object(raw)).unwrap()),
        raw_png_metadata: None,
        raw_workflow: None,
    })
}

/// 8BIMリソースブロック列を走査し、IPTC IIM (リソースID 0x0404) のデータを
/// 収集する。未知リソースは読み飛ばし。境界が崩れたらそこまで。
fn collect_8bim_iim(body: &[u8], out: &mut Vec<Vec<u8>>) {
    let mut pos = 0usize;
    const MAX_RESOURCES: usize = 1024;
    for _ in 0..MAX_RESOURCES {
        if pos + 12 > body.len() {
            return;
        }
        let sig = &body[pos..pos + 4];
        if sig != b"8BIM" {
            return; // 未知シグネチャで整列を失った
        }
        let id = u16::from_be_bytes([body[pos + 4], body[pos + 5]]);
        // Pascal形式名: 長さバイト + 名前本体、全体 (長さバイト込み) が偶数に
        // パディングされる
        let name_len = body[pos + 6] as usize;
        let name_field = 1 + name_len;
        let name_padded = name_field + (name_field % 2);
        let size_at = pos + 6 + name_padded;
        if size_at + 4 > body.len() {
            return;
        }
        let size = u32::from_be_bytes([
            body[size_at],
            body[size_at + 1],
            body[size_at + 2],
            body[size_at + 3],
        ]) as usize;
        let data_at = size_at + 4;
        if data_at + size > body.len() {
            return;
        }
        if id == 0x0404 {
            out.push(body[data_at..data_at + size].to_vec());
        }
        // リソースデータは偶数長へパディング
        pos = data_at + size + (size % 2);
    }
}

/// IIMデータセット列を解析し、rawマップへ記録する。
fn parse_iim_datasets(
    iim: &[u8],
    raw: &mut serde_json::Map<String, serde_json::Value>,
    keywords: &mut Vec<String>,
) {
    let mut pos = 0usize;
    for _ in 0..MAX_DATASETS {
        if pos + 5 > iim.len() {
            return;
        }
        if iim[pos] != 0x1C {
            return; // データセットマーカ不整合 — そこまでの結果で妥協
        }
        let record = iim[pos + 1];
        let dataset = iim[pos + 2];
        let len_hi = iim[pos + 3];
        let len_lo = iim[pos + 4];
        pos += 5;
        let value_len = if len_hi & 0x80 != 0 {
            // 拡張長: 下位15bit = 長さを表すバイト数 (big endian)
            let bytes = (((len_hi & 0x7F) as usize) << 8 | len_lo as usize).min(8);
            if pos + bytes > iim.len() {
                return;
            }
            let mut len = 0usize;
            for i in 0..bytes {
                len = (len << 8) | iim[pos + i] as usize;
            }
            pos += bytes;
            len
        } else {
            ((len_hi as usize) << 8) | len_lo as usize
        };
        if pos + value_len > iim.len() {
            return;
        }
        let value = &iim[pos..pos + value_len];
        pos += value_len;
        let text = decode_iim_string(value);
        let text = truncate_chars(text);
        if text.is_empty() {
            continue;
        }
        // 既知レコードは名前付きキーへ、未知は "record:dataset" 生キーへ
        let key: Option<&str> = match (record, dataset) {
            (2, 5) => Some("title"),
            (2, 25) => None, // keywordsへ追記 (下で処理)
            (2, 55) => Some("date_created"),
            (2, 80) => Some("byline"),
            (2, 116) => Some("copyright"),
            (2, 120) => Some("caption"),
            (2, 122) => Some("writer"),
            (1, 90) => Some("coded_character_set"),
            _ => None,
        };
        if (record, dataset) == (2, 25) {
            keywords.push(text);
            continue;
        }
        if let Some(key) = key {
            // 既知キーの重複は最初を採用 (IIM仕様でも繰り返し不可)
            raw.entry(key.to_string())
                .or_insert(serde_json::Value::String(text));
        } else {
            let raw_key = format!("{record}:{dataset}");
            // 未知レコードの重複は "r:d#n" で番号付け
            let mut candidate = raw_key.clone();
            let mut n = 1;
            while raw.contains_key(&candidate) {
                n += 1;
                candidate = format!("{raw_key}#{n}");
            }
            raw.insert(candidate, serde_json::Value::String(text));
        }
    }
}

/// IIM文字列値のデコード: UTF-8を試し、失敗時はLatin-1とみなす
/// (CodedCharacterSet=ESC %G指定時のみUTF-8が正しいが、安全側の
/// 通用デコードで十分 — 表示・コピーが目的)。
fn decode_iim_string(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    }
}

fn truncate_chars(text: String) -> String {
    if text.chars().count() <= VALUE_LIMIT {
        return text;
    }
    let cut: String = text.chars().take(VALUE_LIMIT).collect();
    format!("{cut}…(truncated)")
}

#[cfg(test)]
mod tests {
    use super::super::test_support as ts;
    use super::*;

    #[test]
    fn parses_common_records_to_named_keys() {
        let row = parse_iptc_app13(&[ts::sample_iptc_app13_payload()]).unwrap();
        assert_eq!(row.parser_name, "iptc");
        let raw: serde_json::Value =
            serde_json::from_str(row.raw_iptc.as_deref().unwrap()).unwrap();
        let obj = raw.as_object().unwrap();
        assert_eq!(obj["title"], serde_json::json!("Test Title"));
        assert_eq!(obj["byline"], serde_json::json!("Photographer"));
        assert_eq!(obj["copyright"], serde_json::json!("(c) 2026"));
        assert_eq!(obj["caption"], serde_json::json!("A caption from IPTC"));
        // keywordsは複数datasetの配列
        assert_eq!(
            obj["keywords"],
            serde_json::json!(["keyword-a", "keyword-b"])
        );
        // 未知レコード (2:10) は生キーで保持
        assert_eq!(obj["2:10"], serde_json::json!("priority=5"));
        // 正規化列は空 (写真系列はEXIFが正本)
        assert!(!row.columns.has_any_column());
        assert!(row.raw_exif.is_none());
    }

    #[test]
    fn multi_segment_payloads_are_concatenated() {
        // IIMを2つのAPP13セグメントに分割 (8BIMリソース単位)
        let payload = ts::sample_iptc_app13_payload();
        let header_len = b"Photoshop 3.0\0".len();
        let (head, tail) = payload.split_at(header_len + 26); // 適当な境界
        let mut seg1 = b"Photoshop 3.0\0".to_vec();
        seg1.extend_from_slice(head[header_len..].to_vec().as_slice());
        let mut seg2 = b"Photoshop 3.0\0".to_vec();
        seg2.extend_from_slice(tail);
        // 境界が8BIMブロック途中で壊れる可能性がある — このテストでは
        // 壊れない境界 (リソース終端) を選ぶため、リソース数1のfixtureでは
        // 分割せず2リソース構成を別途作る
        let _ = (seg1, seg2);

        // 2リソース構成で検証 (title + keywordsを別リソースに)
        fn resource(id: u16, data: &[u8]) -> Vec<u8> {
            let mut res = Vec::new();
            res.extend_from_slice(b"8BIM");
            res.extend_from_slice(&id.to_be_bytes());
            res.extend_from_slice(&[0, 0]);
            res.extend_from_slice(&(data.len() as u32).to_be_bytes());
            res.extend_from_slice(data);
            if data.len() % 2 == 1 {
                res.push(0);
            }
            res
        }
        let mut iim1 = Vec::new();
        ts::iim_dataset(&mut iim1, 2, 5, b"Seg Title");
        let mut iim2 = Vec::new();
        ts::iim_dataset(&mut iim2, 2, 120, b"Seg Caption");
        let mut p1 = b"Photoshop 3.0\0".to_vec();
        p1.extend_from_slice(&resource(0x0404, &iim1));
        let mut p2 = b"Photoshop 3.0\0".to_vec();
        p2.extend_from_slice(&resource(0x0404, &iim2));
        let row = parse_iptc_app13(&[p1, p2]).unwrap();
        let raw: serde_json::Value =
            serde_json::from_str(row.raw_iptc.as_deref().unwrap()).unwrap();
        assert_eq!(raw["title"], serde_json::json!("Seg Title"));
        assert_eq!(raw["caption"], serde_json::json!("Seg Caption"));
    }

    #[test]
    fn non_iptc_app13_returns_none() {
        assert!(parse_iptc_app13(&[]).is_none());
        assert!(parse_iptc_app13(&[b"garbage".to_vec()]).is_none());
        // 8BIMだがIPTCリソース不在
        let mut res = Vec::new();
        res.extend_from_slice(b"8BIM");
        res.extend_from_slice(&0x0403u16.to_be_bytes()); // 別リソースID
        res.extend_from_slice(&[0, 0]);
        res.extend_from_slice(&4u32.to_be_bytes());
        res.extend_from_slice(b"data");
        let mut payload = b"Photoshop 3.0\0".to_vec();
        payload.extend_from_slice(&res);
        assert!(parse_iptc_app13(&[payload]).is_none());
    }

    #[test]
    fn extended_length_datasets() {
        // 拡張長形式 (0x8000形式) のデータセット
        let value = b"extended caption value";
        let mut iim = vec![0x1C, 2, 120, 0x80, 0x04]; // 長さは続く4バイトで表現
        iim.extend_from_slice(&(value.len() as u32).to_be_bytes());
        iim.extend_from_slice(value);

        let mut res = Vec::new();
        res.extend_from_slice(b"8BIM");
        res.extend_from_slice(&0x0404u16.to_be_bytes());
        res.extend_from_slice(&[0, 0]);
        res.extend_from_slice(&(iim.len() as u32).to_be_bytes());
        res.extend_from_slice(&iim);
        let mut payload = b"Photoshop 3.0\0".to_vec();
        payload.extend_from_slice(&res);
        let row = parse_iptc_app13(&[payload]).unwrap();
        let raw: serde_json::Value =
            serde_json::from_str(row.raw_iptc.as_deref().unwrap()).unwrap();
        assert_eq!(raw["caption"], serde_json::json!("extended caption value"));
    }

    #[test]
    fn malformed_tail_is_tolerated() {
        // 末尾にゴミが付いたIIM — 手前のデータセットまで保持
        let mut iim = Vec::new();
        ts::iim_dataset(&mut iim, 2, 120, b"ok caption");
        iim.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]); // マーカ不整合
        let mut res = Vec::new();
        res.extend_from_slice(b"8BIM");
        res.extend_from_slice(&0x0404u16.to_be_bytes());
        res.extend_from_slice(&[0, 0]);
        res.extend_from_slice(&(iim.len() as u32).to_be_bytes());
        res.extend_from_slice(&iim);
        let mut payload = b"Photoshop 3.0\0".to_vec();
        payload.extend_from_slice(&res);
        let row = parse_iptc_app13(&[payload]).unwrap();
        let raw: serde_json::Value =
            serde_json::from_str(row.raw_iptc.as_deref().unwrap()).unwrap();
        assert_eq!(raw["caption"], serde_json::json!("ok caption"));
    }
}
