//! EXIF Orientationの読み取りと表示変換 (MVP WP-3)。
//!
//! - 読み取りは `kamadak-exif` 1本で全コンテナ (JPEG APP1 / TIFF /
//!   PNG eXIf / WebP EXIFチャンク) を扱う (`Reader::read_from_container`)
//! - 適用はデコード後のメモリ上変換のみ。**元ファイルは一切変更しない**
//!   (MVP阻止条件「元画像を意図せず変更」の遵守)
//! - Orientation反映でキャッシュ内容が変わるため `ALGORITHM_VERSION` を
//!   繰り上げる契約 (cache.rs参照)

use std::fs::File;
use std::io::{BufReader, Seek, SeekFrom};
use std::path::Path;

use image::DynamicImage;

/// EXIF Orientation値 (1..=8) を読み取る。EXIF不在・読み取り失敗・
/// 値不正は `None` (無変換扱い)。読み取りはファイル先頭から行い、
/// 呼び出し後の位置は保証しない (呼び出し側で巻き戻す)。
pub fn read_orientation(path: &Path) -> Option<u8> {
    let Ok(mut file) = File::open(path) else {
        return None;
    };
    read_orientation_at(&mut file)
}

/// 開いたfdからEXIF Orientationを読む (プローブ/デコードと同一fd束ね用)。
pub fn read_orientation_at(file: &mut File) -> Option<u8> {
    if file.seek(SeekFrom::Start(0)).is_err() {
        return None;
    }
    let exif = exif::Reader::new()
        .read_from_container(&mut BufReader::new(file))
        .ok()?;
    let field = exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)?;
    let value = field.value.get_uint(0)?;
    u8::try_from(value).ok().filter(|v| (1..=8).contains(v))
}

/// Orientation値 (1..=8) を表示向きへ変換する。1 (無変換)・不正値は
/// そのまま返す。変換の意味論は `image::metadata::Orientation`
/// (`from_exif` + `apply_orientation`) に一任する — 単一の検証済み実装
/// に寄せるため、本crateでは再実装しない。
pub fn apply_orientation(img: DynamicImage, orientation: u8) -> DynamicImage {
    let Some(parsed) = image::metadata::Orientation::from_exif(orientation) else {
        return img;
    };
    let mut img = img;
    img.apply_orientation(parsed);
    img
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::GenericImageView as _;
    use std::fs;

    struct TestFile(std::path::PathBuf);

    impl TestFile {
        fn new(name: &str, bytes: &[u8]) -> Self {
            let path = std::env::temp_dir().join(format!(
                "gento-orient-{}-{}-{}",
                name,
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::write(&path, bytes).unwrap();
            Self(path)
        }
    }

    impl Drop for TestFile {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    /// 非対称な3色マーカー入り画像 (2x2): 左上=赤(255,0,0)、右上=緑、
    /// 左下=青、右下=白。各Orientation変換後の色配置を幾何的に予測する。
    fn marker_image() -> DynamicImage {
        image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(2, 2, |x, y| match (x, y) {
            (0, 0) => image::Rgb([255, 0, 0]),
            (1, 0) => image::Rgb([0, 255, 0]),
            (0, 1) => image::Rgb([0, 0, 255]),
            _ => image::Rgb([255, 255, 255]),
        }))
    }

    fn px(img: &DynamicImage, x: u32, y: u32) -> [u8; 3] {
        let rgba = img.get_pixel(x, y);
        [rgba[0], rgba[1], rgba[2]]
    }

    #[test]
    fn orientation_transform_table_all_values() {
        let img = marker_image();
        // 変換後の色配置の期待値 (EXIF仕様の表示変換。幾何導出):
        // 2=左右反転、3=180°回転、4=上下反転、5=転置 (主対角線で反転)、
        // 6=時計回り90°、7=逆転置 (副対角線で反転)、8=反時計回り90°
        let red = [255, 0, 0];
        let green = [0, 255, 0];
        let blue = [0, 0, 255];
        let white = [255, 255, 255];
        // (orientation, 変換後4ピクセルの色配置)
        type Cell = ((u32, u32), [u8; 3]);
        let table: &[(u8, [Cell; 4])] = &[
            (
                1,
                [
                    ((0, 0), red),
                    ((1, 0), green),
                    ((0, 1), blue),
                    ((1, 1), white),
                ],
            ),
            (
                2,
                [
                    ((0, 0), green),
                    ((1, 0), red),
                    ((0, 1), white),
                    ((1, 1), blue),
                ],
            ),
            (
                3,
                [
                    ((0, 0), white),
                    ((1, 0), blue),
                    ((0, 1), green),
                    ((1, 1), red),
                ],
            ),
            (
                4,
                [
                    ((0, 0), blue),
                    ((1, 0), white),
                    ((0, 1), red),
                    ((1, 1), green),
                ],
            ),
            (
                5,
                [
                    ((0, 0), red),
                    ((1, 0), blue),
                    ((0, 1), green),
                    ((1, 1), white),
                ],
            ),
            (
                6,
                [
                    ((0, 0), blue),
                    ((1, 0), red),
                    ((0, 1), white),
                    ((1, 1), green),
                ],
            ),
            (
                7,
                [
                    ((0, 0), white),
                    ((1, 0), green),
                    ((0, 1), blue),
                    ((1, 1), red),
                ],
            ),
            (
                8,
                [
                    ((0, 0), green),
                    ((1, 0), white),
                    ((0, 1), red),
                    ((1, 1), blue),
                ],
            ),
        ];
        for (orientation, expected) in table {
            let out = apply_orientation(img.clone(), *orientation);
            for ((x, y), want) in expected {
                let got = px(&out, *x, *y);
                assert_eq!(
                    got, *want,
                    "orientation {orientation}: pixel ({x},{y}) color mismatch"
                );
            }
        }

        // 不正値 (0, 9) は無変換
        for bad in [0u8, 9u8] {
            let out = apply_orientation(img.clone(), bad);
            assert_eq!(px(&out, 0, 0), red, "invalid orientation {bad} keeps image");
        }
    }

    /// TIFF EXIFブロック (Orientation=6) を手組みで生成する。
    /// IFD0にOrientation (tag 0x0112, SHORT) のみ置く最小構成。
    fn tiff_exif_bytes(orientation: u16) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&[0x49, 0x49, 0x2A, 0x00]); // TIFF LE
        out.extend_from_slice(&8u32.to_le_bytes()); // IFD0 offset
        out.extend_from_slice(&1u16.to_le_bytes()); // エントリ数
        out.extend_from_slice(&0x0112u16.to_le_bytes()); // tag
        out.extend_from_slice(&3u16.to_le_bytes()); // type SHORT
        out.extend_from_slice(&1u32.to_le_bytes()); // count
        out.extend_from_slice(&orientation.to_le_bytes());
        out.extend_from_slice(&[0u8; 2]); // 4バイト境界パディング
        out.extend_from_slice(&0u32.to_le_bytes()); // 次のIFDなし
        out
    }

    /// JPEG APP1 (Exif) セグメントで包む。
    fn jpeg_with_exif(exif_payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&[0xFF, 0xD8]); // SOI
        let body_len = u16::try_from(exif_payload.len() + 2).unwrap();
        out.extend_from_slice(&[0xFF, 0xE1]); // APP1
        out.extend_from_slice(&body_len.to_be_bytes());
        out.extend_from_slice(exif_payload);
        out.extend_from_slice(&[0xFF, 0xD9]); // EOI
        out
    }

    /// PNG eXIfチャンクとして包む (CRCはpng crate互換の自前計算)。
    fn png_with_exif_chunk(exif_payload: &[u8]) -> Vec<u8> {
        fn crc32(data: &[u8]) -> u32 {
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
        let mut out = Vec::new();
        out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        // まずIHDR (1x1 8bit RGB) のチャンクを書き、続けてeXIfチャンク
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&1u32.to_be_bytes());
        ihdr.extend_from_slice(&1u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        for (kind, data) in [
            (b"IHDR".as_slice(), ihdr.as_slice()),
            (b"eXIf".as_slice(), exif_payload),
        ] {
            let mut chunk = Vec::new();
            chunk.extend_from_slice(kind);
            chunk.extend_from_slice(data);
            let crc = crc32(&chunk);
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            out.extend_from_slice(&chunk);
            out.extend_from_slice(&crc.to_be_bytes());
        }
        out
    }

    /// WebP EXIFチャンクとして包む (RIFF手組み)。
    fn webp_with_exif_chunk(exif_payload: &[u8]) -> Vec<u8> {
        fn chunk(fourcc: &[u8; 4], payload: &[u8]) -> Vec<u8> {
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
        body.extend_from_slice(&chunk(b"VP8X", &[0u8; 10]));
        body.extend_from_slice(&chunk(b"EXIF", exif_payload));
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&((body.len() + 4) as u32).to_le_bytes());
        out.extend_from_slice(b"WEBP");
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn reads_orientation_from_all_four_containers() {
        let tiff = tiff_exif_bytes(6);
        // TIFF単体 (TIFFコンテナとして直接)
        let f = TestFile::new("tiff", &tiff);
        assert_eq!(read_orientation(&f.0), Some(6), "TIFF EXIF");

        // JPEG APP1 (Exifヘッダ + TIFF本体)
        let mut payload = b"Exif\0\0".to_vec();
        payload.extend_from_slice(&tiff_exif_bytes(3));
        let f = TestFile::new("jpeg", &jpeg_with_exif(&payload));
        assert_eq!(read_orientation(&f.0), Some(3), "JPEG APP1");

        // PNG eXIfチャンク
        let f = TestFile::new("png", &png_with_exif_chunk(&tiff_exif_bytes(8)));
        assert_eq!(read_orientation(&f.0), Some(8), "PNG eXIf");

        // WebP EXIFチャンク
        let f = TestFile::new("webp", &webp_with_exif_chunk(&tiff_exif_bytes(5)));
        assert_eq!(read_orientation(&f.0), Some(5), "WebP EXIF");
    }

    #[test]
    fn no_exif_or_invalid_returns_none() {
        // EXIFなしの通常PNG
        let f = TestFile::new("plain-png", &crate::probe::minimal_png_bytes(4, 4));
        assert_eq!(read_orientation(&f.0), None);

        // Orientationタグ不在のEXIF (DateTimeのみ)
        let mut tiff = Vec::new();
        tiff.extend_from_slice(&[0x49, 0x49, 0x2A, 0x00]);
        tiff.extend_from_slice(&8u32.to_le_bytes());
        tiff.extend_from_slice(&0u16.to_le_bytes()); // エントリ0件
        tiff.extend_from_slice(&0u32.to_le_bytes());
        let f = TestFile::new("no-tag", &tiff);
        assert_eq!(read_orientation(&f.0), None);

        // 存在しないパス
        assert_eq!(
            read_orientation(Path::new("/nonexistent-gento/x.jpg")),
            None
        );
    }

    #[test]
    fn real_jpeg_roundtrip_orientation_applied() {
        // 実JPEG (EXIF Orientation=6付き) を生成し、変換が寸法転置と
        // 色配置の両方で成立することを確認する。JPEGは非可逆のため
        // 色は近似判定する。
        let img = marker_image();
        let mut jpeg = Vec::new();
        {
            let mut cursor = std::io::Cursor::new(&mut jpeg);
            img.write_to(&mut cursor, image::ImageFormat::Jpeg)
                .expect("jpeg encode");
        }
        // APP1 (Exif) をSOI直後に挿入
        let mut with_exif = Vec::new();
        with_exif.extend_from_slice(&jpeg[..2]);
        let mut payload = b"Exif\0\0".to_vec();
        payload.extend_from_slice(&tiff_exif_bytes(6));
        with_exif.extend_from_slice(&[0xFF, 0xE1]);
        with_exif.extend_from_slice(&u16::try_from(payload.len() + 2).unwrap().to_be_bytes());
        with_exif.extend_from_slice(&payload);
        with_exif.extend_from_slice(&jpeg[2..]);

        let f = TestFile::new("real-jpeg", &with_exif);
        assert_eq!(read_orientation(&f.0), Some(6));

        let decoded = image::ImageReader::open(&f.0)
            .unwrap()
            .with_guessed_format()
            .unwrap()
            .decode()
            .unwrap();
        assert_eq!((decoded.width(), decoded.height()), (2, 2));
        let oriented = apply_orientation(decoded, 6);
        assert_eq!((oriented.width(), oriented.height()), (2, 2));
        // orientation 6 (時計回り90°): 左上の赤は右上へ移動する
        let top_right = oriented.get_pixel(1, 0);
        assert!(
            top_right[0] > 200 && top_right[1] < 80,
            "赤マーカーが (1,0) へ移動していること: {top_right:?}"
        );
    }
}
