//! テスト用fixture生成器 (WP-9)。
//!
//! すべてRustコード上で手組み生成 (外部ツール・実ファイルに非依存。
//! 決定的に再現可能)。PNGチャンク・TIFF EXIF・JPEGセグメント・IPTC IIM
//! のバイナリ形式を直接構築する。

/// CRC32 (PNGチャンク検証用。probe.rsのテストと同じ実装)。
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// PNGチャンク (len + type + data + CRC)。
pub fn png_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(kind);
    body.extend_from_slice(data);
    let crc = crc32(&body);
    let mut out = Vec::new();
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc.to_be_bytes());
    out
}

/// PNGシグネチャ + IHDR (1x1 8bit RGBA)。
pub fn png_header() -> Vec<u8> {
    let mut png = Vec::new();
    png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&1u32.to_be_bytes());
    ihdr.extend_from_slice(&1u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    png.extend_from_slice(&png_chunk(b"IHDR", &ihdr));
    png
}

// ===== TIFF EXIF本体 (IFD0 + Exif IFD + GPS IFD) =====

/// TIFFエントリ値の種別。
#[derive(Clone)]
pub enum TiffValue {
    Ascii(&'static str),
    Short(u16),
    /// LONG (ポインタ等の生u32値)。
    Long(u32),
    Rationals(Vec<(u32, u32)>),
}

/// IFD定義。
pub struct TiffIfd {
    pub entries: Vec<(u16, TiffValue)>,
}

fn value_type_size(value: &TiffValue) -> (u16, u32, usize) {
    // (TIFF type, count, バイト長)
    match value {
        TiffValue::Ascii(s) => (2, s.len() as u32 + 1, s.len() + 1),
        TiffValue::Short(_) => (3, 1, 2),
        TiffValue::Long(_) => (4, 1, 4),
        TiffValue::Rationals(v) => (5, v.len() as u32, v.len() * 8),
    }
}

/// IFD0 (Make/Model/Orientation) + Exif IFD (DateTimeOriginal/LensModel) +
/// GPS IFD (緯度35°39'31"N / 経度139°44'44"E) の完全なTIFFをLEで組む。
pub fn sample_tiff_exif_with_gps() -> Vec<u8> {
    let ifd0 = TiffIfd {
        entries: vec![
            (0x010F, TiffValue::Ascii("TestCam")),    // Make
            (0x0110, TiffValue::Ascii("Model X100")), // Model
            (0x0112, TiffValue::Short(6)),            // Orientation
        ],
    };
    let exif_ifd = TiffIfd {
        entries: vec![
            (0x9003, TiffValue::Ascii("2026:08:31 12:34:56")), // DateTimeOriginal
            (0xA434, TiffValue::Ascii("23mm f/2")),            // LensModel
        ],
    };
    let gps_ifd = TiffIfd {
        entries: vec![
            (0x0001, TiffValue::Ascii("N")),
            (
                0x0002,
                TiffValue::Rationals(vec![(35, 1), (39, 1), (3100, 100)]),
            ),
            (0x0003, TiffValue::Ascii("E")),
            (
                0x0004,
                TiffValue::Rationals(vec![(139, 1), (44, 1), (4400, 100)]),
            ),
        ],
    };
    build_tiff(&ifd0, &exif_ifd, &gps_ifd)
}

/// Orientationのみの最小TIFF (orientation.rsのテストと同じ形式)。
pub fn tiff_orientation_only(orientation: u16) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&[0x49, 0x49, 0x2A, 0x00]);
    out.extend_from_slice(&8u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&0x0112u16.to_le_bytes());
    out.extend_from_slice(&3u16.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&orientation.to_le_bytes());
    out.extend_from_slice(&[0u8; 2]);
    out.extend_from_slice(&0u32.to_le_bytes());
    out
}

fn build_tiff(ifd0: &TiffIfd, exif_ifd: &TiffIfd, gps_ifd: &TiffIfd) -> Vec<u8> {
    let ifd_size = |n: usize| 2 + n * 12 + 4;
    // IFD0にはExif/GPSポインタ (LONG) が2エントリ追加される
    let ifd0_count = ifd0.entries.len() + 2;
    let ifd0_off = 8usize;
    let exif_off = ifd0_off + ifd_size(ifd0_count);
    let gps_off = exif_off + ifd_size(exif_ifd.entries.len());
    let data_base = gps_off + ifd_size(gps_ifd.entries.len());

    let mut tiff = vec![0u8; data_base];
    tiff[0..2].copy_from_slice(&[0x49, 0x49]);
    tiff[2..4].copy_from_slice(&0x2A_u16.to_le_bytes());
    tiff[4..8].copy_from_slice(&(ifd0_off as u32).to_le_bytes());
    let mut data: Vec<u8> = Vec::new();

    let emit =
        |tiff: &mut Vec<u8>, data: &mut Vec<u8>, base: usize, entries: &[(u16, TiffValue)]| {
            let mut sorted: Vec<&(u16, TiffValue)> = entries.iter().collect();
            sorted.sort_by_key(|(t, _)| *t);
            tiff[base..base + 2].copy_from_slice(&(sorted.len() as u16).to_le_bytes());
            for (i, (tag, value)) in sorted.iter().enumerate() {
                let at = base + 2 + i * 12;
                let (typ, count, len) = value_type_size(value);
                tiff[at..at + 2].copy_from_slice(&tag.to_le_bytes());
                tiff[at + 2..at + 4].copy_from_slice(&typ.to_le_bytes());
                tiff[at + 4..at + 8].copy_from_slice(&count.to_le_bytes());
                match value {
                    TiffValue::Ascii(s) if len <= 4 => {
                        let mut v = s.as_bytes().to_vec();
                        v.push(0);
                        v.resize(4, 0);
                        tiff[at + 8..at + 12].copy_from_slice(&v);
                    }
                    TiffValue::Ascii(s) => {
                        tiff[at + 8..at + 12]
                            .copy_from_slice(&((data_base + data.len()) as u32).to_le_bytes());
                        data.extend_from_slice(s.as_bytes());
                        data.push(0);
                    }
                    TiffValue::Short(v) => {
                        tiff[at + 8..at + 10].copy_from_slice(&v.to_le_bytes());
                    }
                    TiffValue::Long(v) => {
                        tiff[at + 8..at + 12].copy_from_slice(&v.to_le_bytes());
                    }
                    TiffValue::Rationals(values) => {
                        tiff[at + 8..at + 12]
                            .copy_from_slice(&((data_base + data.len()) as u32).to_le_bytes());
                        for (n, d) in values {
                            data.extend_from_slice(&n.to_le_bytes());
                            data.extend_from_slice(&d.to_le_bytes());
                        }
                    }
                }
            }
            // 次IFDオフセット = 0 (未初期化0のまま)
        };

    // IFD0本体 + Exif/GPSポインタ (LONG) — ポインタでExif IFD・GPS IFDを参照
    let mut ifd0_all: Vec<(u16, TiffValue)> = ifd0.entries.clone();
    ifd0_all.push((0x8769, TiffValue::Long(exif_off as u32)));
    ifd0_all.push((0x8825, TiffValue::Long(gps_off as u32)));
    emit(&mut tiff, &mut data, ifd0_off, &ifd0_all);
    emit(&mut tiff, &mut data, exif_off, &exif_ifd.entries);
    emit(&mut tiff, &mut data, gps_off, &gps_ifd.entries);
    tiff.extend_from_slice(&data);
    tiff
}

// ===== コンテナラッパ =====

/// PNG (eXIf + 指定テキストチャンク群)。
pub fn png_with_metadata(text_chunks: &[(&[u8; 4], Vec<u8>)], with_exif: bool) -> Vec<u8> {
    let mut png = png_header();
    if with_exif {
        png.extend_from_slice(&png_chunk(b"eXIf", &sample_tiff_exif_with_gps()));
    }
    for (kind, data) in text_chunks {
        png.extend_from_slice(&png_chunk(kind, data));
    }
    png.extend_from_slice(&png_chunk(b"IEND", &[]));
    png
}

/// eXIf + tEXt 1つ (title) のPNG。
pub fn png_with_text_and_exif() -> Vec<u8> {
    let text = b"AI generated image".to_vec();
    png_with_metadata(&[(b"tEXt", text)], true)
}

/// JPEGセグメント (marker + len + payload)。
pub fn jpeg_segment(marker: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&[0xFF, marker]);
    out.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// SOI + APP1(Exif) + APP13(IPTC) + EOI。実JPEG本体 (image crateで
/// エンコードした2x2) のSOI直後にセグメントを挿入する (orientation.rsの
/// real_jpeg_roundtripと同じ手法 — プローブ可能な実ファイル形式)。
pub fn jpeg_with_exif_and_iptc() -> Vec<u8> {
    let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(2, 2, |x, y| {
        image::Rgb([(x * 120) as u8, (y * 120) as u8, 90])
    }));
    let mut jpeg = Vec::new();
    {
        let mut cursor = std::io::Cursor::new(&mut jpeg);
        img.write_to(&mut cursor, image::ImageFormat::Jpeg)
            .expect("jpeg encode");
    }
    let mut out = Vec::new();
    out.extend_from_slice(&jpeg[..2]); // SOI
    let mut exif_payload = b"Exif\0\0".to_vec();
    exif_payload.extend_from_slice(&sample_tiff_exif_with_gps());
    out.extend_from_slice(&jpeg_segment(0xE1, &exif_payload));
    out.extend_from_slice(&jpeg_segment(0xED, &sample_iptc_app13_payload()));
    out.extend_from_slice(&jpeg[2..]);
    out
}

// ===== IPTC IIM (APP13 8BIM) =====

/// IIMデータセット1件を追加。
pub fn iim_dataset(iim: &mut Vec<u8>, record: u8, ds: u8, value: &[u8]) {
    iim.push(0x1C);
    iim.push(record);
    iim.push(ds);
    iim.extend_from_slice(&(value.len() as u16).to_be_bytes());
    iim.extend_from_slice(value);
}

/// APP13 8BIM + IIM (title/keywords×2/byline/copyright/caption + 未知dataset)。
pub fn sample_iptc_app13_payload() -> Vec<u8> {
    let mut iim = Vec::new();
    iim_dataset(&mut iim, 2, 5, b"Test Title");
    iim_dataset(&mut iim, 2, 25, b"keyword-a");
    iim_dataset(&mut iim, 2, 25, b"keyword-b");
    iim_dataset(&mut iim, 2, 80, b"Photographer");
    iim_dataset(&mut iim, 2, 116, b"(c) 2026");
    iim_dataset(&mut iim, 2, 120, b"A caption from IPTC");
    // 未知レコード (raw保持の確認用)
    iim_dataset(&mut iim, 2, 10, b"priority=5");

    let mut res = Vec::new();
    res.extend_from_slice(b"8BIM");
    res.extend_from_slice(&0x0404u16.to_be_bytes());
    res.extend_from_slice(&[0, 0]); // 空Pascal名 (長さ0 + パディング)
    res.extend_from_slice(&(iim.len() as u32).to_be_bytes());
    res.extend_from_slice(&iim);
    if iim.len() % 2 == 1 {
        res.push(0); // リソースデータは偶数長へパディング
    }

    let mut payload = Vec::new();
    payload.extend_from_slice(b"Photoshop 3.0\0");
    payload.extend_from_slice(&res);
    payload
}

// ===== zlib (zTXt/iTXt compressed用) =====

/// 最小zlibストリーム (stored deflate — 圧縮なし)。
/// 展開結果は `data` そのまま。zTXtテストで使用。
pub fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(0x78); // CMF: deflate 32KiB窓
    out.push(0x01); // FLG: check bits (0x7801はzlibとして有効)
                    // storedブロック: 最終フラグ + LEN + NLEN + 生データ (5バイトごとに分割)
    let mut chunks = data.chunks(65_535).peekable();
    if data.is_empty() {
        out.push(0x01);
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(!0u16).to_le_bytes());
    }
    while let Some(chunk) = chunks.next() {
        let last = chunks.peek().is_none();
        out.push(u8::from(last));
        out.extend_from_slice(&(chunk.len() as u16).to_le_bytes());
        out.extend_from_slice(&(!(chunk.len() as u16)).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    // Adler-32
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    out.extend_from_slice(&((b << 16) | a).to_be_bytes());
    out
}
