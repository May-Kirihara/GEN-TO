//! サムネイル・previewの生成 (Phase 4 / WP-3)。
//!
//! パイプライン: デコード (`image` crate、Limits付きでdecompression bomb対策)
//! → リサイズ (長辺基準・アスペクト維持・拡大なし)
//! → エンコード (`webp` crate、lossy quality 80、明示的RGBAバッファ渡し)
//! → キャッシュ書き込み (一時ファイル+アトミックrename)。
//!
//! OrientationはPNG対象外のため不変 (ALGORITHM_VERSIONに織り込み済み)。

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use gento_core::SizeClass;
use image::{DynamicImage, ImageError, ImageReader};
use webp::Encoder as WebpEncoder;

use crate::cache::{asset_fingerprint, cache_key, AssetFingerprint, ThumbnailCache};
use crate::pool::{CancelToken, Job};
use crate::stats::ThumbStats;

/// WebP lossy品質。
pub const WEBP_QUALITY: f32 = 80.0;

/// デコード割り当て上限 (バイト)。データセット最大7.9MP (=RGBA 32MiB) に対し
/// 十分広い安全域を持ちつつ、decompression bomb (億画素級) を遮断する。
pub const MAX_DECODE_ALLOC_BYTES: u64 = 256 * 1024 * 1024;

/// 異常寸法の安全弁 (strict limit。上限を1辺あたりで制限する)。
pub const MAX_DECODE_DIMENSION: u32 = 20_000;

/// 生成エラー。
#[derive(Debug)]
pub enum ThumbnailError {
    /// オープン不能。
    Unreadable(String),
    /// デコード失敗 (形式不正・切り詰め・上限超過を含む)。
    Decode(ImageError),
    /// WebPエンコード失敗。
    Encode(String),
    /// 協調キャンセル。
    Cancelled,
}

impl fmt::Display for ThumbnailError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ThumbnailError::Unreadable(reason) => write!(f, "unreadable: {reason}"),
            ThumbnailError::Decode(err) => write!(f, "decode failed: {err}"),
            ThumbnailError::Encode(reason) => write!(f, "encode failed: {reason}"),
            ThumbnailError::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl std::error::Error for ThumbnailError {}

/// 生成成果物。
#[derive(Debug, Clone)]
pub struct ThumbnailOutput {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// PNG (形式は推定) を上限付きでデコードする。
pub fn decode_png_with_limits(path: &Path) -> Result<DynamicImage, ThumbnailError> {
    let mut reader = ImageReader::open(path)
        .map_err(|e| ThumbnailError::Unreadable(e.to_string()))?
        .with_guessed_format()
        .map_err(|e| ThumbnailError::Unreadable(e.to_string()))?;
    reader.limits(decode_limits());
    reader.decode().map_err(ThumbnailError::Decode)
}

/// デコード制限の構成。
pub fn decode_limits() -> image::Limits {
    // Limitsは#[non_exhaustive]のためDefault + フィールド代入で構成する
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DECODE_DIMENSION);
    limits.max_image_height = Some(MAX_DECODE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_ALLOC_BYTES);
    limits
}

/// 長辺が `long_edge` を超える場合のみ縮小する (拡大しない)。
/// 出力寸法は厳密に (nw, nh)。アスペクト比は長辺=long_edgeを基準に切り上げで保つ。
pub fn resize_no_upscale(img: DynamicImage, size_class: SizeClass) -> DynamicImage {
    let long_edge = size_class.long_edge();
    let (w, h) = (img.width(), img.height());
    let long = w.max(h);
    if long <= long_edge {
        return img;
    }
    let (nw, nh) = target_dimensions(w, h, long_edge);
    img.thumbnail_exact(nw, nh)
}

/// 長辺を `long_edge` に合わせた出力寸法 (切り上げ、最低1px)。
pub fn target_dimensions(w: u32, h: u32, long_edge: u32) -> (u32, u32) {
    let long = u64::from(w.max(h));
    let (nw, nh) = if w >= h {
        (
            u64::from(long_edge),
            (u64::from(h) * u64::from(long_edge)).div_ceil(long),
        )
    } else {
        (
            (u64::from(w) * u64::from(long_edge)).div_ceil(long),
            u64::from(long_edge),
        )
    };
    (
        nw.clamp(1, u64::from(long_edge)) as u32,
        nh.clamp(1, u64::from(long_edge)) as u32,
    )
}

/// RGBAバッファをWebP lossyでエンコードする (image feature結合に依存しない
/// 明示的RGBA渡し。PM計画 §3)。
pub fn encode_webp(img: &DynamicImage) -> Result<Vec<u8>, ThumbnailError> {
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    let encoder = WebpEncoder::from_rgba(rgba.as_raw(), w, h);
    let encoded = encoder.encode(WEBP_QUALITY);
    if encoded.is_empty() {
        return Err(ThumbnailError::Encode("empty webp output".to_string()));
    }
    Ok(encoded.to_vec())
}

/// デコード→リサイズ→エンコード。ステージ間でキャンセルを検査する。
pub fn generate(
    path: &Path,
    size_class: SizeClass,
    cancel: &CancelToken,
) -> Result<ThumbnailOutput, ThumbnailError> {
    if cancel.is_cancelled() {
        return Err(ThumbnailError::Cancelled);
    }
    let decoded = decode_png_with_limits(path)?;
    if cancel.is_cancelled() {
        return Err(ThumbnailError::Cancelled);
    }
    let resized = resize_no_upscale(decoded, size_class);
    if cancel.is_cancelled() {
        return Err(ThumbnailError::Cancelled);
    }
    let (width, height) = (resized.width(), resized.height());
    let bytes = encode_webp(&resized)?;
    Ok(ThumbnailOutput {
        bytes,
        width,
        height,
    })
}

/// プールへ投入するサムネイル生成ジョブ。キャッシュ書き込みまで行う。
///
/// fingerprintは要求時点のカタログ値 (size+mtime) を使い、要求時のlookupキーと
/// 同一キーへ書き込む (ファイルが要求後に変わっても要求時キーへ保存する)。
pub struct ThumbnailJob {
    pub source: PathBuf,
    pub fingerprint: AssetFingerprint,
    pub size_class: SizeClass,
    pub cache: Arc<ThumbnailCache>,
    pub stats: Arc<ThumbStats>,
}

impl Job for ThumbnailJob {
    fn run(&self, cancel: &CancelToken) -> Result<Vec<u8>, String> {
        let started = Instant::now();
        let output = generate(&self.source, self.size_class, cancel).map_err(|e| e.to_string())?;
        if cancel.is_cancelled() {
            // エンコード完了後のキャンセル: 書き込まずに中断する
            return Err("cancelled".to_string());
        }
        let key = cache_key(&self.fingerprint, self.size_class);
        let writer = self
            .cache
            .begin_write(&key)
            .map_err(|e| format!("cache write prepare failed: {e}"))?;
        let bytes_written = writer
            .commit(&output.bytes)
            .map_err(|e| format!("cache commit failed: {e}"))?;
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        self.stats.record_generated(elapsed_ms, bytes_written);
        tracing::debug!(
            source = %self.source.display(),
            size_class = self.size_class.ipc_name(),
            dims = ?(output.width, output.height),
            bytes = output.bytes.len(),
            elapsed_ms = format_args!("{elapsed_ms:.1}"),
            "thumbnail generated"
        );
        Ok(output.bytes)
    }
}

/// テスト・呼び出し側の便宜: パスからfingerprintを作るラッパー。
pub fn fingerprint_of(path: &Path, byte_size: u64, mtime_ms: u64) -> AssetFingerprint {
    asset_fingerprint(path, byte_size, mtime_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU32, Ordering};

    use crate::cache::ThumbnailCache;
    use crate::pool::Outcome;

    static DIR_SEQ: AtomicU32 = AtomicU32::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let seq = DIR_SEQ.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "gento-thumb-test-{}-{}-{}",
                name,
                std::process::id(),
                seq
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// テスト用PNGを書く。transparent=trueなら左上4分の1を透過にする。
    fn write_test_png(dir: &TestDir, name: &str, w: u32, h: u32, transparent: bool) -> PathBuf {
        let img = DynamicImage::ImageRgba8(image::RgbaImage::from_fn(w, h, |x, y| {
            let alpha = if transparent && x < w / 4 && y < h / 4 {
                0
            } else {
                255
            };
            image::Rgba([
                (x % 256) as u8,
                (y % 256) as u8,
                ((x + y) % 256) as u8,
                alpha,
            ])
        }));
        let path = dir.path().join(name);
        img.save(&path).unwrap();
        path
    }

    /// WebPバイト列をデコードして寸法・透過を検査する。
    fn decode_webp(bytes: &[u8]) -> webp::WebPImage {
        webp::Decoder::new(bytes).decode().expect("webp decodable")
    }

    #[test]
    fn target_dimensions_computations() {
        // 横長
        assert_eq!(target_dimensions(600, 400, 256), (256, 171));
        assert_eq!(target_dimensions(600, 400, 512), (512, 342));
        // 縦長
        assert_eq!(target_dimensions(400, 600, 256), (171, 256));
        // 極端な縦長でも最低1px
        assert_eq!(target_dimensions(1000, 1, 256), (256, 1));
        // 正方形
        assert_eq!(target_dimensions(300, 300, 256), (256, 256));
    }

    #[test]
    fn resize_does_not_upscale_small_images() {
        let small = DynamicImage::new_rgba8(64, 32);
        let out = resize_no_upscale(small, SizeClass::S256);
        assert_eq!((out.width(), out.height()), (64, 32));

        let small2 = DynamicImage::new_rgba8(100, 200);
        let out2 = resize_no_upscale(small2, SizeClass::Preview2048);
        assert_eq!((out2.width(), out2.height()), (100, 200));
    }

    #[test]
    fn resize_downscales_large_images_on_long_edge() {
        let img = DynamicImage::new_rgba8(600, 400);
        let s256 = resize_no_upscale(img.clone(), SizeClass::S256);
        assert_eq!((s256.width(), s256.height()), (256, 171));
        let s512 = resize_no_upscale(img.clone(), SizeClass::S512);
        assert_eq!((s512.width(), s512.height()), (512, 342));
        // Preview2048は600x400より大きいため拡大しない
        let preview = resize_no_upscale(img, SizeClass::Preview2048);
        assert_eq!((preview.width(), preview.height()), (600, 400));
    }

    #[test]
    fn generate_small_transparent_png_keeps_dims_and_alpha() {
        let dir = TestDir::new("small-alpha");
        let png = write_test_png(&dir, "a.png", 64, 32, true);
        let out = generate(&png, SizeClass::S256, &CancelToken::new()).unwrap();
        // 拡大しない → 64x32維持
        assert_eq!((out.width, out.height), (64, 32));
        let decoded = decode_webp(&out.bytes);
        assert_eq!((decoded.width(), decoded.height()), (64, 32));
        assert!(decoded.is_alpha(), "alpha channel must survive");
        // 透過ピクセル (0,0) は透過のまま、不透明領域は不透明のまま
        let alpha_tl = decoded[0];
        let alpha_br =
            decoded[(4 * (decoded.width() as usize * decoded.height() as usize - 1)) + 3];
        assert!(alpha_tl <= 32, "transparent corner alpha={alpha_tl}");
        assert!(alpha_br >= 224, "opaque corner alpha={alpha_br}");
    }

    #[test]
    fn generate_large_png_downscales_for_each_class() {
        let dir = TestDir::new("large");
        let png = write_test_png(&dir, "big.png", 600, 400, false);
        let s256 = generate(&png, SizeClass::S256, &CancelToken::new()).unwrap();
        assert_eq!((s256.width, s256.height), (256, 171));
        let s512 = generate(&png, SizeClass::S512, &CancelToken::new()).unwrap();
        assert_eq!((s512.width, s512.height), (512, 342));
        let preview = generate(&png, SizeClass::Preview2048, &CancelToken::new()).unwrap();
        assert_eq!((preview.width, preview.height), (600, 400));
    }

    #[test]
    fn generate_tall_png_resizes_on_height() {
        let dir = TestDir::new("tall");
        let png = write_test_png(&dir, "tall.png", 400, 600, false);
        let out = generate(&png, SizeClass::S256, &CancelToken::new()).unwrap();
        assert_eq!((out.width, out.height), (171, 256));
    }

    #[test]
    fn webp_round_trip_preserves_dimensions_and_alpha_range() {
        let dir = TestDir::new("roundtrip");
        let png = write_test_png(&dir, "rt.png", 300, 150, true);
        let out = generate(&png, SizeClass::S256, &CancelToken::new()).unwrap();
        assert_eq!((out.width, out.height), (256, 128)); // 300x150 → 256x128
        let decoded = decode_webp(&out.bytes);
        assert_eq!((decoded.width(), decoded.height()), (256, 128));
        // 中央 (不透明) と左上 (透過) の両方を検証
        let w = decoded.width() as usize;
        let opaque = decoded[4 * ((decoded.height() as usize / 2) * w + w / 2) + 3];
        let transparent = decoded[0];
        assert!(opaque >= 224);
        assert!(transparent <= 32);
    }

    #[test]
    fn garbage_file_fails_as_decode_error() {
        let dir = TestDir::new("garbage");
        let path = dir.path().join("g.png");
        fs::write(&path, b"definitely not a png").unwrap();
        let err = generate(&path, SizeClass::S256, &CancelToken::new()).unwrap_err();
        assert!(matches!(err, ThumbnailError::Decode(_)), "{err}");
        assert!(err.to_string().contains("decode failed"));
    }

    #[test]
    fn missing_file_fails_as_unreadable() {
        let path = std::env::temp_dir().join("gento-thumb-definitely-missing.png");
        let err = generate(&path, SizeClass::S256, &CancelToken::new()).unwrap_err();
        assert!(matches!(err, ThumbnailError::Unreadable(_)), "{err}");
    }

    #[test]
    fn precancelled_token_short_circuits() {
        let dir = TestDir::new("cancelled");
        let png = write_test_png(&dir, "c.png", 64, 64, false);
        let token = CancelToken::new();
        token.cancel();
        let err = generate(&png, SizeClass::S256, &token).unwrap_err();
        assert!(matches!(err, ThumbnailError::Cancelled));
    }

    /// 異常寸法IHDR (30000x30000を主張する最小PNG)。Limitsで遮断されることを検証。
    fn bomb_png_bytes() -> Vec<u8> {
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
        fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
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
        let mut png = Vec::new();
        png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&30_000u32.to_be_bytes());
        ihdr.extend_from_slice(&30_000u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        png.extend_from_slice(&chunk(b"IHDR", &ihdr));
        png.extend_from_slice(&chunk(b"IEND", &[]));
        png
    }

    #[test]
    fn dimension_limit_blocks_decompression_bomb_header() {
        let dir = TestDir::new("bomb");
        let path = dir.path().join("bomb.png");
        fs::write(&path, bomb_png_bytes()).unwrap();
        let err = generate(&path, SizeClass::S256, &CancelToken::new()).unwrap_err();
        match &err {
            ThumbnailError::Decode(ImageError::Limits(limit)) => {
                // DimensionErrorのDisplayは "Image size exceeds limit"
                let msg = format!("{limit}");
                assert!(msg.contains("exceeds limit"), "{msg}");
            }
            other => panic!("expected limits error, got: {other}"),
        }
    }

    #[test]
    fn thumbnail_job_writes_cache_and_records_stats() {
        let dir = TestDir::new("job");
        let png = write_test_png(&dir, "j.png", 600, 400, false);
        let stats = Arc::new(ThumbStats::new());
        let cache =
            Arc::new(ThumbnailCache::open(dir.path().join("cache"), stats.clone()).unwrap());

        let md = fs::metadata(&png).unwrap();
        let fingerprint = fingerprint_of(
            &png,
            md.len(),
            md.modified()
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
        );
        let job = ThumbnailJob {
            source: png.clone(),
            fingerprint: fingerprint.clone(),
            size_class: SizeClass::S256,
            cache: Arc::clone(&cache),
            stats: Arc::clone(&stats),
        };
        let bytes = job.run(&CancelToken::new()).unwrap();
        assert!(!bytes.is_empty());

        let key = crate::cache::cache_key(&fingerprint, SizeClass::S256);
        assert!(matches!(
            cache.lookup(&key),
            crate::cache::CacheLookup::Hit(_)
        ));
        let snap = stats.snapshot();
        assert_eq!(snap.generated, 1);
        assert!(snap.bytes_written > 0);
        assert!(snap.p50_ms.is_some());

        // 同一ジョブをpool経由で実行しても同様に完了する (結合確認)
        let (pool, rx) = crate::pool::WorkerPool::new(crate::pool::PoolConfig {
            worker_count: 1,
            queue_capacity: 8,
        });
        let job2 = ThumbnailJob {
            source: png,
            fingerprint: fingerprint.clone(),
            size_class: SizeClass::S512,
            cache,
            stats,
        };
        let handle = pool
            .submit("job2", gento_core::Priority::Interactive, Arc::new(job2))
            .unwrap()
            .handle();
        assert!(matches!(handle.wait(), Outcome::Completed(_)));
        let completions: Vec<_> = rx.try_iter().collect();
        assert_eq!(completions.len(), 1);
        pool.shutdown();
    }
}
