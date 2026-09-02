//! 送信前の画像縮小 (MVP WP-10。spec §16「送信前に画像を縮小する」)。
//!
//! 代表ファイルをデコードし、最大辺を `max_edge` (既定1024px) に
//! 縮小してJPEG (quality 85) へ再エンコードしたうえでdata URL化する。
//! EXIF等のメタデータは再エンコードで除去される (GPS位置情報の
//! 意図しない添付を構造的に防ぐ — 送信可否はプロンプト側の制御)。
//!
//! デコード制限はサムネイル経路 (gento-image) と同じ値
//! (寸法20000・割り当て256MiB) を適用する。
//!
//! `max_edge` は呼び出し値に関わらず [MIN_IMAGE_EDGE, MAX_IMAGE_EDGE]
//! へクランプされる — 外部へ出る画像の最大辺は構造的に2048pxまで。

use base64::Engine;
use image::{DynamicImage, ImageReader, Limits};

/// 送信画像の最大辺の下限 (px)。
pub const MIN_IMAGE_EDGE: u32 = 64;
/// 送信画像の最大辺の上限 (px)。原寸送信を構造的に防止する境界。
pub const MAX_IMAGE_EDGE: u32 = 2048;
pub const DEFAULT_MAX_EDGE: u32 = 1024;
pub const JPEG_QUALITY: u8 = 85;

/// テスト機能 (test_vision) 用の1x1 PNG。
pub const TINY_PNG_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

/// 縮小結果。
pub struct EncodedImage {
    pub data_url: String,
    pub width: u32,
    pub height: u32,
    pub jpeg_bytes: usize,
}

/// 画像バイト列を (必要なら) 縮小しJPEG data URLへ変換する。
///
/// - `max_edge` は [MIN_IMAGE_EDGE, MAX_IMAGE_EDGE] へクランプされる
///   (実行経路の最終防衛。0や2048超が伝わっていても原寸は出ない)。
///- 既に長辺が `max_edge` 以下なら拡大しない。
pub fn encode_for_send(bytes: &[u8], max_edge: u32) -> Result<EncodedImage, String> {
    let max_edge = clamp_image_edge(max_edge);
    let mut limits = Limits::default();
    limits.max_image_width = Some(20_000);
    limits.max_image_height = Some(20_000);
    limits.max_alloc = Some(256 * 1024 * 1024);
    let mut reader = ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    reader.limits(limits);
    let decoded = reader.decode().map_err(|e| format!("decode failed: {e}"))?;
    let (orig_w, orig_h) = (decoded.width(), decoded.height());

    let resized = if orig_w.max(orig_h) > max_edge {
        resize_long_edge(&decoded, max_edge)
    } else {
        decoded
    };
    let (w, h) = (resized.width(), resized.height());

    let mut jpeg_bytes: Vec<u8> = Vec::new();
    let rgb = DynamicImage::ImageRgb8(convert_to_rgb8(&resized));
    {
        let encoder =
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_bytes, JPEG_QUALITY);
        rgb.write_with_encoder(encoder)
            .map_err(|e| format!("jpeg encode failed: {e}"))?;
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(&jpeg_bytes);
    Ok(EncodedImage {
        data_url: format!("data:image/jpeg;base64,{b64}"),
        width: w,
        height: h,
        jpeg_bytes: jpeg_bytes.len(),
    })
}

/// 長辺が `edge` になるよう縮小 (短辺はアスペクト保持。
/// `thumbnail` = 高速な整数box平均。拡大はしない)。
fn resize_long_edge(image: &DynamicImage, edge: u32) -> DynamicImage {
    image.thumbnail(edge, edge)
}

/// 送信画像の最大辺を実効範囲 [MIN_IMAGE_EDGE, MAX_IMAGE_EDGE] へ
/// クランプする (全実行経路の最終防衛)。
pub fn clamp_image_edge(edge: u32) -> u32 {
    edge.clamp(MIN_IMAGE_EDGE, MAX_IMAGE_EDGE)
}

/// 透過を持つ形式のRGBA→白背景合成。JPEGは透過を表現できないため
/// (アルファ欠落で黒背景になるのを防ぐ)。
fn convert_to_rgb8(image: &DynamicImage) -> image::RgbImage {
    match image {
        DynamicImage::ImageRgb8(rgb) => rgb.clone(),
        DynamicImage::ImageRgba8(rgba) => {
            let mut rgb = image::RgbImage::new(rgba.width(), rgba.height());
            for (x, y, pixel) in rgba.enumerate_pixels() {
                let [r, g, b, a] = pixel.0;
                let alpha = a as u32;
                let blend =
                    |c: u8| -> u8 { ((c as u32 * alpha + 255 * (255 - alpha)) / 255) as u8 };
                rgb.put_pixel(x, y, image::Rgb([blend(r), blend(g), blend(b)]));
            }
            rgb
        }
        other => other.to_rgb8(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    fn png_bytes(w: u32, h: u32) -> Vec<u8> {
        let img = RgbImage::from_fn(w, h, |x, _| Rgb([(x % 256) as u8, 0x80, 0x40]));
        let mut out = Vec::new();
        img.write_with_encoder(image::codecs::png::PngEncoder::new(&mut out))
            .expect("encode png");
        out
    }

    #[test]
    fn huge_image_is_downscaled_to_max_edge() {
        // 4000x2000 → 長辺1024 (1024x512)
        let bytes = png_bytes(4000, 2000);
        let encoded = encode_for_send(&bytes, 1024).unwrap();
        assert_eq!(encoded.width, 1024);
        assert_eq!(encoded.height, 512);
        assert!(encoded.data_url.starts_with("data:image/jpeg;base64,"));
        assert!(encoded.jpeg_bytes > 0);
        // 実際にJPEGとしてデコード可能
        let decoded = image::load_from_memory(
            &base64::engine::general_purpose::STANDARD
                .decode(
                    encoded
                        .data_url
                        .trim_start_matches("data:image/jpeg;base64,"),
                )
                .unwrap(),
        )
        .unwrap();
        assert_eq!((decoded.width(), decoded.height()), (1024, 512));
    }

    #[test]
    fn small_image_is_not_upscaled() {
        // 64x32 → そのまま
        let bytes = png_bytes(64, 32);
        let encoded = encode_for_send(&bytes, 1024).unwrap();
        assert_eq!((encoded.width, encoded.height), (64, 32));
    }

    #[test]
    fn max_edge_out_of_range_is_clamped_never_full_resolution() {
        // 0 (旧「縮小なし」) は下限64へ、2048超は上限2048へクランプ —
        // 原寸が外部へ出る経路を構造的に塞ぐ (FG-WP10 blocker 1)。
        let zero = encode_for_send(&png_bytes(4000, 2000), 0).unwrap();
        assert_eq!((zero.width, zero.height), (64, 32), "0は下限64へ");
        let huge = encode_for_send(&png_bytes(4000, 2000), 8192).unwrap();
        assert_eq!(huge.width, MAX_IMAGE_EDGE, "8192は上限2048へ");
        assert_eq!(huge.height, 1024);
        // 範囲内はそのまま (拡大しない)
        let small = encode_for_send(&png_bytes(64, 32), 2048).unwrap();
        assert_eq!((small.width, small.height), (64, 32));
        assert_eq!(clamp_image_edge(u32::MAX), MAX_IMAGE_EDGE);
        assert_eq!(clamp_image_edge(1), MIN_IMAGE_EDGE);
    }

    #[test]
    fn corrupt_input_is_rejected_not_panicking() {
        assert!(encode_for_send(b"not an image", 1024).is_err());
        assert!(encode_for_send(&[], 1024).is_err());
    }

    #[test]
    fn portrait_image_limits_long_edge() {
        // 縦長 1000x3000 → 長辺1024で 341x1024 (切り捨て許容)
        let bytes = png_bytes(1000, 3000);
        let encoded = encode_for_send(&bytes, 1024).unwrap();
        assert_eq!(encoded.height, 1024);
        assert!(encoded.width <= 342 && encoded.width >= 340);
    }
}
