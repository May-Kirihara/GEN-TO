//! EXIF解析 (WP-9)。撮影日時・カメラ/レンズ・GPS・Orientationを正規化し、
//! 全フィールドをraw_exif JSONへ。

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::sync::atomic::Ordering;

use exif::{Exif, Field, Tag, Value};
use gento_core::assetmeta::{AssetMetadataParsed, MetadataColumns};

use super::{METADATA_READ_LIMIT_BYTES, PARSER_EXIF};

/// raw JSONの1フィールド値文字列表現の上限 (巨大MakerNote等の保護)。
const RAW_FIELD_LIMIT: usize = 2048;
/// raw JSONへ収めるフィールド数の上限。
const RAW_FIELD_COUNT: usize = 1024;

/// TIFF本体バイト列 (コンテナ抽出済み) を解析する。
/// 不正・フィールドなしはNone。
pub(crate) fn parse_exif_tiff_bytes(tiff: Option<&[u8]>) -> Option<AssetMetadataParsed> {
    let tiff = tiff?;
    // TIFFヘッダ (II/MM + 0x2A) の確認 — 誤ったチャンクを弾く
    if tiff.len() < 8 {
        return None;
    }
    if !matches!(&tiff[..2], b"II" | b"MM") {
        return None;
    }
    let exif = exif::Reader::new().read_raw(tiff.to_vec()).ok()?;
    build_row(&exif)
}

/// 開いたfdからコンテナ経由 (TIFF本体 / WebP EXIFチャンク) で解析する。
/// 読み取り総量は `METADATA_READ_LIMIT_BYTES` で制限。
pub(crate) fn parse_exif_container(file: &mut File) -> Option<AssetMetadataParsed> {
    if file.rewind().is_err() {
        return None;
    }
    // ReadLimiterにSeekが無いため、crateのコンテナ抽出が必要とする
    // BufRead + Seekを両立させるラッパ: 読み取りは制限付きで透過し、
    // seekは元のfdへ素通す (seek自体は読み取り量を消費しない)。
    struct LimitedSeekable<'a> {
        file: &'a File,
        remaining: std::sync::atomic::AtomicU64,
    }
    impl Read for LimitedSeekable<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let remaining = self.remaining.load(Ordering::Relaxed);
            if remaining == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "metadata read limit exceeded",
                ));
            }
            let cap = buf.len().min(remaining as usize);
            let read = self.file.read(&mut buf[..cap])?;
            self.remaining.fetch_sub(read as u64, Ordering::Relaxed);
            Ok(read)
        }
    }
    impl Seek for LimitedSeekable<'_> {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            self.file.seek(pos)
        }
    }
    let mut reader = BufReader::new(LimitedSeekable {
        file,
        remaining: std::sync::atomic::AtomicU64::new(METADATA_READ_LIMIT_BYTES),
    });
    let exif = exif::Reader::new().read_from_container(&mut reader).ok()?;
    build_row(&exif)
}

/// 解析済みExif → 行 (フィールドが1つもなければNone)。
fn build_row(exif: &Exif) -> Option<AssetMetadataParsed> {
    let mut columns = MetadataColumns::default();
    let mut raw = serde_json::Map::new();
    let mut taken_at: Option<String> = None;
    let mut gps_lat: Option<f64> = None;
    let mut gps_lat_ref: Option<String> = None;
    let mut gps_lon: Option<f64> = None;
    let mut gps_lon_ref: Option<String> = None;

    let fields: Vec<&Field> = exif.fields().take(RAW_FIELD_COUNT + 1).collect();
    if fields.is_empty() {
        return None;
    }
    for field in &fields {
        let key = format!("{}::{}", field.ifd_num, field.tag);
        raw.insert(key, value_to_json(&field.value));
        match field.tag {
            Tag::Make => columns.camera_make = ascii_value(&field.value),
            Tag::Model => columns.camera_model = ascii_value(&field.value),
            Tag::LensModel => columns.lens = ascii_value(&field.value),
            Tag::DateTimeOriginal | Tag::DateTime => {
                if taken_at.is_none() {
                    taken_at = ascii_value(&field.value).and_then(|s| exif_datetime_to_rfc3339(&s));
                }
            }
            Tag::Orientation => {
                columns.orientation = field.value.get_uint(0).map(|v| v as i64);
            }
            Tag::GPSLatitude => gps_lat = dms_to_decimal(&field.value),
            Tag::GPSLatitudeRef => gps_lat_ref = ascii_value(&field.value),
            Tag::GPSLongitude => gps_lon = dms_to_decimal(&field.value),
            Tag::GPSLongitudeRef => gps_lon_ref = ascii_value(&field.value),
            _ => {}
        }
    }
    if fields.len() > RAW_FIELD_COUNT {
        raw.insert(
            "__warnings".to_string(),
            serde_json::json!([format!(
                "フィールド数が多いため先頭{RAW_FIELD_COUNT}件のみ保持しました"
            )]),
        );
    }
    columns.taken_at = taken_at;
    // GPSは緯度・経度・方向が揃って初めて意味を持つ (片側のみは破棄)
    if let (Some(mut lat), Some(lat_ref), Some(mut lon), Some(lon_ref)) =
        (gps_lat, gps_lat_ref, gps_lon, gps_lon_ref)
    {
        if lat_ref.starts_with('S') {
            lat = -lat;
        }
        if lon_ref.starts_with('W') {
            lon = -lon;
        }
        columns.gps_latitude = Some(lat);
        columns.gps_longitude = Some(lon);
    }
    Some(AssetMetadataParsed {
        parser_name: PARSER_EXIF.to_string(),
        parser_version: "1".to_string(),
        columns,
        raw_exif: Some(serde_json::to_string(&serde_json::Value::Object(raw)).unwrap()),
        raw_iptc: None,
        raw_png_metadata: None,
        raw_workflow: None,
    })
}

/// ASCII値 → 前後空白除去したString (非ASCIIバイトもlossyで保持)。
fn ascii_value(value: &Value) -> Option<String> {
    if let Value::Ascii(components) = value {
        let first = components.first()?;
        let text = String::from_utf8_lossy(first).trim().to_string();
        if text.is_empty() {
            return None;
        }
        return Some(text);
    }
    None
}

/// EXIF日時 "YYYY:MM:DD HH:MM:SS" → "YYYY-MM-DDTHH:MM:SS"。
/// 桁・区切りが仕様と異なる場合はNone (検証せず通さない)。
fn exif_datetime_to_rfc3339(text: &str) -> Option<String> {
    let b = text.as_bytes();
    if b.len() != 19 {
        return None;
    }
    // 形式検証: YYYY:MM:DD HH:MM:SS
    let ok = b.len() == 19
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b':'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b':'
        && b[8..10].iter().all(u8::is_ascii_digit)
        && b[10] == b' '
        && b[11..13].iter().all(u8::is_ascii_digit)
        && b[13] == b':'
        && b[14..16].iter().all(u8::is_ascii_digit)
        && b[16] == b':'
        && b[17..19].iter().all(u8::is_ascii_digit);
    if !ok {
        return None;
    }
    let mut out = text.to_string();
    // 日付区切り2つのコロンを'-'へ、日時間の空白を'T'へ
    unsafe {
        out.as_bytes_mut()[4] = b'-';
        out.as_bytes_mut()[7] = b'-';
        out.as_bytes_mut()[10] = b'T';
    }
    Some(out)
}

/// GPS DMS (Rational列) → 十進度。成分数2 (度, 分) にも対応。
fn dms_to_decimal(value: &Value) -> Option<f64> {
    if let Value::Rational(rationals) = value {
        if rationals.is_empty() || rationals.len() > 3 {
            return None;
        }
        let mut out = 0f64;
        for (i, r) in rationals.iter().enumerate() {
            if r.denom == 0 {
                return None;
            }
            out += r.to_f64() / 60f64.powi(i as i32);
        }
        return Some(out);
    }
    None
}

/// Value → raw JSON値 (文字列表現は上限付き)。
fn value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Ascii(components) => {
            let rendered = components
                .iter()
                .map(|c| String::from_utf8_lossy(c).into_owned())
                .collect::<Vec<_>>()
                .join("\\0");
            truncate_json_string(rendered)
        }
        Value::Byte(v) => {
            truncate_json_string(v.iter().map(|b| format!("{b:02x}")).collect::<String>())
        }
        Value::Undefined(v, _) => truncate_json_string(String::from_utf8_lossy(v).into_owned()),
        Value::Short(v) => numbers_to_json(v.iter().map(|x| *x as i64)),
        Value::Long(v) => numbers_to_json(v.iter().map(|x| *x as i64)),
        Value::SShort(v) => numbers_to_json(v.iter().map(|x| *x as i64)),
        Value::SLong(v) => numbers_to_json(v.iter().map(|x| *x as i64)),
        Value::Rational(v) => {
            let items = v
                .iter()
                .map(|r| serde_json::json!({"num": r.num, "denom": r.denom}));
            serde_json::Value::Array(items.collect())
        }
        Value::SRational(v) => {
            let items = v
                .iter()
                .map(|r| serde_json::json!({"num": r.num, "denom": r.denom}));
            serde_json::Value::Array(items.collect())
        }
        Value::Float(v) => numbers_to_json(v.iter().map(|x| *x as i64)),
        Value::Double(v) => numbers_to_json(v.iter().map(|x| *x as i64)),
        Value::SByte(v) => {
            truncate_json_string(v.iter().map(|b| format!("{b:02x}")).collect::<String>())
        }
        Value::Unknown(..) => serde_json::Value::Null,
    }
}

fn numbers_to_json<'a>(values: impl Iterator<Item = i64> + 'a) -> serde_json::Value {
    let collected: Vec<serde_json::Value> = values.map(serde_json::Value::from).collect();
    if collected.len() == 1 {
        collected.into_iter().next().unwrap()
    } else {
        serde_json::Value::Array(collected)
    }
}

/// 文字列のchars上限 (バイト上限ではなく文字境界で切る)。
fn truncate_json_string(text: String) -> serde_json::Value {
    if text.chars().count() <= RAW_FIELD_LIMIT {
        return serde_json::Value::String(text);
    }
    let cut: String = text.chars().take(RAW_FIELD_LIMIT).collect();
    serde_json::Value::String(format!("{cut}…(truncated)"))
}

#[cfg(test)]
mod tests {
    use super::super::test_support as ts;
    use super::*;

    #[test]
    fn normalizes_photo_columns_from_tiff() {
        let row = parse_exif_tiff_bytes(Some(&ts::sample_tiff_exif_with_gps())).unwrap();
        assert_eq!(row.parser_name, "exif");
        assert_eq!(row.parser_version, "1");
        let c = &row.columns;
        assert_eq!(c.camera_make.as_deref(), Some("TestCam"));
        assert_eq!(c.camera_model.as_deref(), Some("Model X100"));
        assert_eq!(c.lens.as_deref(), Some("23mm f/2"));
        assert_eq!(c.taken_at.as_deref(), Some("2026-08-31T12:34:56"));
        assert_eq!(c.orientation, Some(6));
        // 35°39'31" N / 139°44'44" E
        let lat = c.gps_latitude.unwrap();
        let lon = c.gps_longitude.unwrap();
        assert!((lat - 35.6586).abs() < 1e-3, "lat={lat}");
        assert!((lon - 139.7455).abs() < 1e-3, "lon={lon}");
        // raw JSONに主要フィールドが入っている
        let raw: serde_json::Value =
            serde_json::from_str(row.raw_exif.as_deref().unwrap()).unwrap();
        assert!(raw.as_object().unwrap().len() >= 8);
    }

    #[test]
    fn raw_json_contains_all_fields_with_ifd_keys() {
        let row = parse_exif_tiff_bytes(Some(&ts::sample_tiff_exif_with_gps())).unwrap();
        let raw: serde_json::Value =
            serde_json::from_str(row.raw_exif.as_deref().unwrap()).unwrap();
        let obj = raw.as_object().unwrap();
        // IFD名::タグ名形式のキー (例: "primary::Make")
        assert!(obj.keys().any(|k| k.ends_with("::Make")));
        assert!(obj.keys().any(|k| k.ends_with("::GPSLatitude")));
        assert!(obj.keys().any(|k| k.ends_with("::DateTimeOriginal")));
    }

    #[test]
    fn orientation_only_tiff_still_yields_row() {
        let row = parse_exif_tiff_bytes(Some(&ts::tiff_orientation_only(8))).unwrap();
        assert_eq!(row.columns.orientation, Some(8));
        assert!(row.columns.taken_at.is_none());
    }

    #[test]
    fn invalid_tiff_returns_none() {
        assert!(parse_exif_tiff_bytes(Some(b"")).is_none());
        assert!(parse_exif_tiff_bytes(Some(b"nope1234")).is_none());
        assert!(parse_exif_tiff_bytes(None).is_none());
    }

    #[test]
    fn exif_datetime_validation() {
        assert_eq!(
            exif_datetime_to_rfc3339("2026:08:31 12:34:56").as_deref(),
            Some("2026-08-31T12:34:56")
        );
        assert_eq!(
            exif_datetime_to_rfc3339("2026-08-31T12:34:56"),
            None,
            "既にRFC3339形はEXIF形でない"
        );
        assert_eq!(exif_datetime_to_rfc3339("bogus"), None);
        assert_eq!(exif_datetime_to_rfc3339("2026:08:3112:34:56"), None);
    }

    #[test]
    fn gps_partial_is_dropped() {
        // GPSLatitudeのみ (Ref無し) は破棄される — 片側のみ・方向不明の
        // 座標は誤解を招くため。IFD0→GPS IFDの最小構成を手組みする。
        let mut tiff = Vec::new();
        tiff.extend_from_slice(&[0x49, 0x49, 0x2A, 0x00]);
        tiff.extend_from_slice(&8u32.to_le_bytes());
        tiff.extend_from_slice(&1u16.to_le_bytes());
        // tag 0x8825 (GPSInfoPointer) — type LONG, offset = 26 (IFD0後)
        tiff.extend_from_slice(&0x8825u16.to_le_bytes());
        tiff.extend_from_slice(&4u16.to_le_bytes());
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&26u32.to_le_bytes());
        tiff.extend_from_slice(&0u32.to_le_bytes()); // next IFD
                                                     // GPS IFD at 26
        tiff.extend_from_slice(&1u16.to_le_bytes());
        tiff.extend_from_slice(&0x0002u16.to_le_bytes());
        tiff.extend_from_slice(&5u16.to_le_bytes());
        tiff.extend_from_slice(&3u32.to_le_bytes());
        tiff.extend_from_slice(&40u32.to_le_bytes()); // データoffset
        tiff.extend_from_slice(&0u32.to_le_bytes());
        // rationals at 40
        for (n, d) in [(35u32, 1u32), (39, 1), (3100, 100)] {
            tiff.extend_from_slice(&n.to_le_bytes());
            tiff.extend_from_slice(&d.to_le_bytes());
        }
        let row = parse_exif_tiff_bytes(Some(&tiff)).unwrap();
        assert!(row.columns.gps_latitude.is_none(), "Ref無しは座標破棄");
        // rawにはGPSLatitude自体は残る (未知項目もRaw保持)
        let raw: serde_json::Value =
            serde_json::from_str(row.raw_exif.as_deref().unwrap()).unwrap();
        assert!(raw
            .as_object()
            .unwrap()
            .keys()
            .any(|k| k.ends_with("::GPSLatitude")));
    }

    #[test]
    fn south_west_coordinates_are_negative() {
        // S/W参照 → 負の十進度
        let mut tiff = ts::sample_tiff_exif_with_gps();
        // N→S, E→W へ書き換え (GPS IFD内のAscii "N"/"E" を探す)
        // データ領域は末尾にあるASCII文字列を直接書き換える
        for i in 0..tiff.len() {
            if tiff[i] == b'N' && tiff[i + 1] == 0 {
                tiff[i] = b'S';
            }
        }
        for i in 0..tiff.len() {
            if tiff[i] == b'E' && tiff[i + 1] == 0 {
                tiff[i] = b'W';
            }
        }
        let row = parse_exif_tiff_bytes(Some(&tiff)).unwrap();
        assert!(row.columns.gps_latitude.unwrap() < 0.0);
        assert!(row.columns.gps_longitude.unwrap() < 0.0);
    }

    #[test]
    fn container_path_tiff_and_webp() {
        let dir = std::env::temp_dir().join(format!(
            "gento-exif-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // TIFF単体ファイル
        let tiff_path = dir.join("a.tiff");
        std::fs::write(&tiff_path, ts::sample_tiff_exif_with_gps()).unwrap();
        let mut file = File::open(&tiff_path).unwrap();
        let row = parse_exif_container(&mut file).unwrap();
        assert_eq!(row.columns.camera_make.as_deref(), Some("TestCam"));

        // WebP EXIFチャンク
        fn webp_chunk(fourcc: &[u8; 4], payload: &[u8]) -> Vec<u8> {
            let mut out = Vec::new();
            out.extend_from_slice(fourcc);
            out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            out.extend_from_slice(payload);
            if payload.len() % 2 == 1 {
                out.push(0);
            }
            out
        }
        let mut body: Vec<u8> = Vec::new();
        body.extend_from_slice(&webp_chunk(b"VP8X", &[0u8; 10]));
        body.extend_from_slice(&webp_chunk(b"EXIF", &ts::sample_tiff_exif_with_gps()));
        let mut webp = Vec::new();
        webp.extend_from_slice(b"RIFF");
        webp.extend_from_slice(&((body.len() + 4) as u32).to_le_bytes());
        webp.extend_from_slice(b"WEBP");
        webp.extend_from_slice(&body);
        let webp_path = dir.join("a.webp");
        std::fs::write(&webp_path, &webp).unwrap();
        let mut file = File::open(&webp_path).unwrap();
        let row = parse_exif_container(&mut file).unwrap();
        assert_eq!(row.columns.camera_model.as_deref(), Some("Model X100"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
