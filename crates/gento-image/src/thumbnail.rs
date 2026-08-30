//! サムネイル・previewの生成 (Phase 4 / MVP WP-3)。
//!
//! パイプライン: デコード (`image` crate、magic判定+Limits付きで
//! decompression bomb対策) → EXIF Orientation適用 (メモリ上変換のみ。
//! 元ファイル不変) → リサイズ (長辺基準・アスペクト維持・拡大なし)
//! → エンコード (`webp` crate、lossy quality 80、明示的RGBAバッファ渡し)
//! → キャッシュ書き込み (一時ファイル+アトミックrename)。
//!
//! アニメーションGIF/WebPは**先頭フレーム**が静止化される (一覧=静止画の
//! 仕様。ビューアのネイティブ再生はoriginal表示で行う)。
//! Orientation反映で出力が変わるため `ALGORITHM_VERSION` は2 (v1キャッシュ
//! はキー不一致で自然無効化)。

use std::fmt;
use std::io::Seek as _;
use std::path::{Path, PathBuf};
#[cfg(any(debug_assertions, feature = "devtools"))]
use std::sync::atomic::Ordering;
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

// ===== 開発用エラー注入 (devtools機能。debug/devtoolsビルドのみ) =====
// 通常releaseビルドからは完全に除去される (WP-0)。

/// 開発用エラー注入のモード (WP-7 / フェーズ8「耐障害性」)。
#[cfg(any(debug_assertions, feature = "devtools"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectionMode {
    None,
    Decode,
    CacheWrite,
}

#[cfg(any(debug_assertions, feature = "devtools"))]
impl InjectionMode {
    pub const IPC_NAMES: [&'static str; 3] = ["none", "decode", "cache_write"];

    pub const fn as_str(self) -> &'static str {
        match self {
            InjectionMode::None => "none",
            InjectionMode::Decode => "decode",
            InjectionMode::CacheWrite => "cache_write",
        }
    }

    pub fn from_ipc(s: &str) -> Option<Self> {
        match s {
            "none" => Some(InjectionMode::None),
            "decode" => Some(InjectionMode::Decode),
            "cache_write" => Some(InjectionMode::CacheWrite),
            _ => None,
        }
    }
}

/// 決定的エラー注入の設定。テストと計測が再現可能であることを保証するため、
/// 乱数を使わずジョブのキャッシュキーhexを閾値判定に使う。
///
/// 判定対象がassetIdではなくキャッシュキー (fingerprint由来) である点は
/// WP-7仕様からの意図的な逸脱: ジョブはキャッシュキーでdedupされるため
/// 1ジョブが複数の仮想assetIdに対応し得る (5万件仮想カタログ)。
/// キャッシュキーは実ファイル+size+mtimeから決まるため同じ決定性を持ち、
/// 実カタログ (1ファイル=1 asset) ではassetIdハッシュと同値になる。
#[cfg(any(debug_assertions, feature = "devtools"))]
#[derive(Debug, Default)]
pub struct FailureInjection {
    mode: std::sync::atomic::AtomicU8,
    /// rate [0.0, 1.0] を1e6スケールの整数閾値として保持 (f64のatomicが無いため)。
    rate_micro: std::sync::atomic::AtomicU64,
}

#[cfg(any(debug_assertions, feature = "devtools"))]
impl FailureInjection {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, mode: InjectionMode, rate: f64) {
        let mode_u8 = match mode {
            InjectionMode::None => 0u8,
            InjectionMode::Decode => 1,
            InjectionMode::CacheWrite => 2,
        };
        self.mode.store(mode_u8, Ordering::Relaxed);
        self.rate_micro.store(
            ((rate.clamp(0.0, 1.0)) * 1_000_000.0) as u64,
            Ordering::Relaxed,
        );
    }

    pub fn mode(&self) -> InjectionMode {
        match self.mode.load(Ordering::Relaxed) {
            1 => InjectionMode::Decode,
            2 => InjectionMode::CacheWrite,
            _ => InjectionMode::None,
        }
    }

    pub fn rate(&self) -> f64 {
        self.rate_micro.load(Ordering::Relaxed) as f64 / 1_000_000.0
    }

    /// このジョブキーに失敗を注入するか。決定的 (同一キーは常に同一判定)。
    pub fn should_inject(&self, key_hex: &str) -> Option<InjectionMode> {
        let mode = self.mode();
        let rate = self.rate();
        if mode == InjectionMode::None || rate <= 0.0 {
            return None;
        }
        if rate >= 1.0 {
            return Some(mode);
        }
        let head = &key_hex[..key_hex.len().min(16)];
        let Ok(value) = u64::from_str_radix(head, 16) else {
            return None;
        };
        if value % 1_000_000 < (rate * 1_000_000.0) as u64 {
            Some(mode)
        } else {
            None
        }
    }
}

/// 生成成果物。
#[derive(Debug, Clone)]
pub struct ThumbnailOutput {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// ファイル (形式はmagic判定) を上限付きでデコードし、EXIF Orientationを
/// 適用して返す。アニメーションは先頭フレーム。
///
/// 形式判定は拡張子ではなく**magic**で行う (拡張子詐称の一貫扱い)。
pub fn decode_with_limits(path: &Path) -> Result<DynamicImage, ThumbnailError> {
    let mut file =
        std::fs::File::open(path).map_err(|e| ThumbnailError::Unreadable(e.to_string()))?;
    let mut head = [0u8; 64];
    let read =
        head_read(&mut file, &mut head).map_err(|e| ThumbnailError::Unreadable(e.to_string()))?;
    let format = crate::format::sniff_format(&head[..read]).ok_or_else(|| {
        ThumbnailError::Decode(ImageError::Unsupported(
            image::error::UnsupportedError::from_format_and_kind(
                image::error::ImageFormatHint::Unknown,
                image::error::UnsupportedErrorKind::Format(image::error::ImageFormatHint::Unknown),
            ),
        ))
    })?;
    // Orientationはデコードと同一fdから読む (読み取り後にrewindする)
    let orientation = crate::orientation::read_orientation_at(&mut file);
    file.rewind()
        .map_err(|e| ThumbnailError::Unreadable(e.to_string()))?;
    let mut reader = ImageReader::with_format(
        std::io::BufReader::new(file),
        crate::format::image_format_of(format),
    );
    reader.limits(decode_limits());
    let decoded = reader.decode().map_err(ThumbnailError::Decode)?;
    match orientation {
        Some(value) => Ok(crate::orientation::apply_orientation(decoded, value)),
        None => Ok(decoded),
    }
}

/// EOFは成功 (短いファイル) として扱う先頭読み取り。
fn head_read(file: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match std::io::Read::read(file, &mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
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
    let decoded = decode_with_limits(path)?;
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
    /// 開発用エラー注入 (WP-7)。注入なしでも全ジョブが共通の設定を参照する。
    /// devtoolsビルドのみ存在するフィールド (WP-0)。
    #[cfg(any(debug_assertions, feature = "devtools"))]
    pub injection: Arc<FailureInjection>,
}

impl Job for ThumbnailJob {
    fn run(&self, cancel: &CancelToken) -> Result<Vec<u8>, String> {
        let started = Instant::now();
        let key = cache_key(&self.fingerprint, self.size_class);
        #[cfg(any(debug_assertions, feature = "devtools"))]
        if self.injection.should_inject(&key.hex) == Some(InjectionMode::Decode) {
            // キュー全体は停止しない: このジョブだけ個別failedになる
            return Err("decode failed: injected failure (decode)".to_string());
        }
        let output = generate(&self.source, self.size_class, cancel).map_err(|e| e.to_string())?;
        if cancel.is_cancelled() {
            // エンコード完了後のキャンセル: 書き込まずに中断する
            return Err("cancelled".to_string());
        }
        #[cfg(any(debug_assertions, feature = "devtools"))]
        if self.injection.should_inject(&key.hex) == Some(InjectionMode::CacheWrite) {
            // 実際に一時ファイルを作ってから失敗させる (Dropガードの後始末を通す)。
            // これにより「注入時も一時ファイルを残さない」ことを検証できる。
            let writer = self
                .cache
                .begin_write(&key)
                .map_err(|e| format!("cache write prepare failed: {e}"))?;
            drop(writer);
            return Err("cache commit failed: injected failure (cache_write)".to_string());
        }
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

    /// MVP各形式 (AVIF除く6形式) の実ファイルからサムネイルが生成できる。
    #[test]
    fn generate_supports_all_mvp_formats() {
        let dir = TestDir::new("formats");
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(400, 260, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 77])
        }));
        for (name, format) in [
            ("a.png", image::ImageFormat::Png),
            ("a.jpg", image::ImageFormat::Jpeg),
            ("a.bmp", image::ImageFormat::Bmp),
            ("a.tiff", image::ImageFormat::Tiff),
        ] {
            let path = dir.path().join(name);
            img.save_with_format(&path, format)
                .unwrap_or_else(|e| panic!("{name} encode: {e}"));
            let out = generate(&path, SizeClass::S256, &CancelToken::new())
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!((out.width, out.height), (256, 167), "{name}");
        }

        // GIF (1フレーム)
        let gif_path = dir.path().join("a.gif");
        {
            let mut buf = Vec::new();
            {
                let mut encoder = image::codecs::gif::GifEncoder::new(&mut buf);
                encoder
                    .encode_frame(image::Frame::new(image::RgbaImage::from_fn(
                        400,
                        260,
                        |x, y| image::Rgba([(x % 256) as u8, (y % 256) as u8, 77, 255]),
                    )))
                    .unwrap();
            }
            fs::write(&gif_path, &buf).unwrap();
        }
        let out = generate(&gif_path, SizeClass::S256, &CancelToken::new()).unwrap();
        assert_eq!((out.width, out.height), (256, 167), "gif");

        // WebP (lossless)
        let webp_path = dir.path().join("a.webp");
        {
            let rgba = image::RgbaImage::from_fn(400, 260, |x, y| {
                image::Rgba([(x % 256) as u8, (y % 256) as u8, 77, 255])
            });
            let encoded = webp::Encoder::from_rgba(rgba.as_raw(), rgba.width(), rgba.height())
                .encode_lossless();
            fs::write(&webp_path, &*encoded).unwrap();
        }
        let out = generate(&webp_path, SizeClass::S256, &CancelToken::new()).unwrap();
        assert_eq!((out.width, out.height), (256, 167), "webp");
    }

    /// アニメーションGIFは先頭フレームで静止化される (一覧=静止画の仕様)。
    #[test]
    fn animated_gif_thumbnail_is_static_first_frame() {
        use image::codecs::gif::{GifEncoder, Repeat};
        use image::{Delay, Frame};
        let dir = TestDir::new("anim-gif");
        let path = dir.path().join("anim.gif");
        {
            let mut buf = Vec::new();
            {
                let mut encoder = GifEncoder::new(&mut buf);
                encoder.set_repeat(Repeat::Infinite).unwrap();
                for i in 0..3u8 {
                    let frame = image::RgbaImage::from_fn(300, 200, |x, y| {
                        // フレームごとに左上の色が変わる (先頭フレーム判定用)
                        let marker = if x < 40 && y < 40 { 255 - i * 60 } else { 30 };
                        image::Rgba([marker, marker, marker, 255])
                    });
                    encoder
                        .encode_frame(Frame::from_parts(
                            frame,
                            0,
                            0,
                            Delay::from_numer_denom_ms(20, 1),
                        ))
                        .unwrap();
                }
            }
            fs::write(&path, &buf).unwrap();
        }
        // プローブ: アニメーション検出
        match crate::format::probe_image(&path) {
            crate::format::ImageProbe::Ok(p) => {
                assert!(p.is_animated);
                assert_eq!(p.frame_count, Some(3));
            }
            other => panic!("probe: {other:?}"),
        }
        // サムネイル: 先頭フレーム (marker=255) の静的WebP
        let out = generate(&path, SizeClass::S256, &CancelToken::new()).unwrap();
        assert_eq!((out.width, out.height), (256, 171));
        let decoded = decode_webp(&out.bytes);
        let px = decoded[0]; // 左上 = marker
        assert!(px > 200, "先頭フレーム (白マーカー) であること: {px}");
    }

    /// EXIF Orientation=6 のJPEGサムネイルは時計回り90°回転されて
    /// 出力される (幅高さの転置で検証)。
    #[test]
    fn oriented_jpeg_thumbnail_applies_rotation() {
        let dir = TestDir::new("orient");
        let path = dir.path().join("o.jpg");
        // 400x200 (横長) のJPEGを作り、Orientation=6 (時計回り90°) を
        // APP1セグメントで挿入する — 適用後は 200x400 (縦長)
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(400, 200, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 9])
        }));
        let mut jpeg = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut jpeg),
            image::ImageFormat::Jpeg,
        )
        .unwrap();
        // IFD0にOrientation (tag 0x0112, SHORT) のみのTIFF EXIFブロック
        let mut tiff = Vec::new();
        tiff.extend_from_slice(&[0x49, 0x49, 0x2A, 0x00]);
        tiff.extend_from_slice(&8u32.to_le_bytes());
        tiff.extend_from_slice(&1u16.to_le_bytes());
        tiff.extend_from_slice(&0x0112u16.to_le_bytes());
        tiff.extend_from_slice(&3u16.to_le_bytes());
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&6u16.to_le_bytes());
        tiff.extend_from_slice(&[0u8; 2]);
        tiff.extend_from_slice(&0u32.to_le_bytes());
        let mut payload = b"Exif\0\0".to_vec();
        payload.extend_from_slice(&tiff);
        let mut with_exif = Vec::new();
        with_exif.extend_from_slice(&jpeg[..2]);
        with_exif.extend_from_slice(&[0xFF, 0xE1]);
        with_exif.extend_from_slice(&u16::try_from(payload.len() + 2).unwrap().to_be_bytes());
        with_exif.extend_from_slice(&payload);
        with_exif.extend_from_slice(&jpeg[2..]);
        fs::write(&path, &with_exif).unwrap();

        let out = generate(&path, SizeClass::S256, &CancelToken::new()).unwrap();
        // 400x200 → orientation 6 → 200x400 → S256 → (128, 256)
        assert_eq!((out.width, out.height), (128, 256));
        // Orientationなしの同画像は (256, 128) — 適用済みであることの対証
        let plain = dir.path().join("plain.jpg");
        fs::write(&plain, &jpeg).unwrap();
        let out2 = generate(&plain, SizeClass::S256, &CancelToken::new()).unwrap();
        assert_eq!((out2.width, out2.height), (256, 128));
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
    fn jpeg_dimension_bomb_is_rejected_by_limits() {
        // SOF0で65500x65500を主張するJPEGヘッダ (データ本体は最小)。
        // デコード時のtotal_bytes予約 (256MiB上限) または寸法検査で拒否される。
        let mut jpeg = Vec::new();
        jpeg.extend_from_slice(&[0xFF, 0xD8]); // SOI
        jpeg.extend_from_slice(&[0xFF, 0xE0, 0, 4, b'J', b'F']); // APP0 (短い)
                                                                 // SOF0: 精度1 + 高さ2 + 幅2 (65500) + コンポーネント数1 + [id, 0x11, 0]
        let mut sof = vec![8u8];
        sof.extend_from_slice(&65_500u16.to_be_bytes());
        sof.extend_from_slice(&65_500u16.to_be_bytes());
        sof.extend_from_slice(&[1, 1, 0x11, 0]);
        jpeg.extend_from_slice(&[0xFF, 0xC0]);
        jpeg.extend_from_slice(&(u16::try_from(sof.len() + 2).unwrap()).to_be_bytes());
        jpeg.extend_from_slice(&sof);
        jpeg.extend_from_slice(&[0xFF, 0xD9]); // EOI
        let dir = TestDir::new("bomb-jpeg");
        let path = dir.path().join("bomb.jpg");
        fs::write(&path, &jpeg).unwrap();
        let err = generate(&path, SizeClass::S256, &CancelToken::new()).unwrap_err();
        // いずれの経路でもErrであれば「過大確保なし」の要件は満たす
        // (Limits or Decoding)。panicしないこと自体が本質。
        assert!(matches!(err, ThumbnailError::Decode(_)), "{err}");
    }

    /// 固定シードの簡易LCG (依存追加なしの変異源。format.rsと同一方式)。
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0
        }
    }

    #[test]
    fn mutated_inputs_never_panic_decode() {
        // 各形式の正当ファイルを固定シードLCGで変異させ、decode_with_limits
        // が常にErrまたは成功のどちらかで**panicしない**ことを検証する
        // (fuzz相当のスモーク。全成功/全失敗は要求しない)。
        let dir = TestDir::new("fuzz");
        let mut seeds: Vec<PathBuf> = Vec::new();

        let png = write_test_png(&dir, "seed.png", 48, 32, true);
        seeds.push(png);

        let jpg = dir.path().join("seed.jpg");
        image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(48, 32, |x, y| {
            image::Rgb([(x * 5 % 256) as u8, (y * 3 % 256) as u8, 42])
        }))
        .save(&jpg)
        .unwrap();
        seeds.push(jpg);

        let gif = dir.path().join("seed.gif");
        {
            let mut buf = Vec::new();
            {
                let mut encoder = image::codecs::gif::GifEncoder::new(&mut buf);
                encoder
                    .encode_frame(image::Frame::new(image::RgbaImage::from_fn(
                        48,
                        32,
                        |x, y| image::Rgba([x as u8, y as u8, 9, 255]),
                    )))
                    .unwrap();
            }
            fs::write(&gif, &buf).unwrap();
        }
        seeds.push(gif);

        let bmp = dir.path().join("seed.bmp");
        image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(48, 32, |x, y| {
            image::Rgb([x as u8, y as u8, 5])
        }))
        .save(&bmp)
        .unwrap();
        seeds.push(bmp);

        let tiff = dir.path().join("seed.tiff");
        image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(48, 32, |x, y| {
            image::Rgb([x as u8, y as u8, 6])
        }))
        .save(&tiff)
        .unwrap();
        seeds.push(tiff);

        let webp = dir.path().join("seed.webp");
        {
            let rgba =
                image::RgbaImage::from_fn(48, 32, |x, y| image::Rgba([x as u8, y as u8, 7, 255]));
            let encoded = webp::Encoder::from_rgba(rgba.as_raw(), rgba.width(), rgba.height())
                .encode_lossless();
            fs::write(&webp, &*encoded).unwrap();
        }
        seeds.push(webp);

        let mut rng = Lcg(0x5EED_0000_FFFF_0001);
        for (si, seed) in seeds.iter().enumerate() {
            let bytes = fs::read(seed).unwrap();
            for i in 0..48u32 {
                let mut mutated = bytes.clone();
                let flips = (rng.next() % 5) + 1;
                for _ in 0..flips {
                    let pos = (rng.next() as usize) % mutated.len();
                    mutated[pos] ^= ((rng.next() % 255) + 1) as u8;
                }
                let mut truncated = mutated.clone();
                let cut = (rng.next() as usize) % (truncated.len() + 1);
                truncated.truncate(cut);
                for (label, payload) in [("mut", &mutated), ("trunc", &truncated)] {
                    let path = dir.path().join(format!(
                        "f-{si}-{i}-{label}{}",
                        seed.extension().unwrap().to_string_lossy()
                    ));
                    fs::write(&path, payload).unwrap();
                    let result = std::panic::catch_unwind(|| {
                        decode_with_limits(&path).map(|img| (img.width(), img.height()))
                    });
                    assert!(result.is_ok(), "{label} must not panic");
                    // generate全体 (リサイズ+エンコードまで) もpanicしない
                    let result = std::panic::catch_unwind(|| {
                        generate(&path, SizeClass::S256, &CancelToken::new()).is_ok()
                    });
                    assert!(result.is_ok(), "{label} generate must not panic");
                }
            }
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
            #[cfg(any(debug_assertions, feature = "devtools"))]
            injection: Arc::new(FailureInjection::new()),
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
            #[cfg(any(debug_assertions, feature = "devtools"))]
            injection: Arc::new(FailureInjection::new()),
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

    // ===== エラー注入 (WP-7 / フェーズ8。devtools機能) =====

    /// rate=0.5のとき閾値の両側に落ちる合成パスを探索する (決定性検証の素材)。
    #[cfg(any(debug_assertions, feature = "devtools"))]
    fn find_split_paths(rate: f64) -> (PathBuf, PathBuf) {
        let mut below = None;
        let mut above = None;
        for i in 0..10_000u32 {
            let fp = fingerprint_of(Path::new(&format!("/inject/{i}.png")), 1000, i as u64);
            let key = crate::cache::cache_key(&fp, SizeClass::S256);
            let head = &key.hex[..16];
            let value = u64::from_str_radix(head, 16).unwrap();
            let injects = value % 1_000_000 < (rate * 1_000_000.0) as u64;
            if injects && below.is_none() {
                below = Some(PathBuf::from(format!("/inject/{i}.png")));
            }
            if !injects && above.is_none() {
                above = Some(PathBuf::from(format!("/inject/{i}.png")));
            }
            if below.is_some() && above.is_some() {
                break;
            }
        }
        (
            below.expect("inject-side path"),
            above.expect("non-inject-side path"),
        )
    }

    #[cfg(any(debug_assertions, feature = "devtools"))]
    fn key_hex_of(path: &Path) -> String {
        let fp = fingerprint_of(path, 1000, 0);
        crate::cache::cache_key(&fp, SizeClass::S256).hex
    }

    #[cfg(any(debug_assertions, feature = "devtools"))]
    #[test]
    fn injection_boundaries_none_and_full() {
        let inj = FailureInjection::new();
        let hex = key_hex_of(Path::new("/inject/boundary.png"));
        assert_eq!(inj.should_inject(&hex), None, "default must not inject");

        inj.set(InjectionMode::Decode, 0.0);
        assert_eq!(inj.should_inject(&hex), None);
        inj.set(InjectionMode::CacheWrite, 1.0);
        assert_eq!(inj.should_inject(&hex), Some(InjectionMode::CacheWrite));
        assert_eq!(inj.mode(), InjectionMode::CacheWrite);
        assert_eq!(inj.rate(), 1.0);

        // 範囲外rateはクランプされる (壊れた入力でも全域/無効にならない)
        inj.set(InjectionMode::Decode, 1.5);
        assert_eq!(inj.should_inject(&hex), Some(InjectionMode::Decode));
        inj.set(InjectionMode::Decode, -0.2);
        assert_eq!(inj.should_inject(&hex), None);
    }

    #[cfg(any(debug_assertions, feature = "devtools"))]
    #[test]
    fn injection_is_deterministic_per_key() {
        let inj = FailureInjection::new();
        inj.set(InjectionMode::Decode, 0.5);
        let (yes, no) = find_split_paths(0.5);
        let yes_hex = key_hex_of(&yes);
        let no_hex = key_hex_of(&no);
        // 同一キーは何度判定しても同じ結果
        for _ in 0..5 {
            assert_eq!(inj.should_inject(&yes_hex), Some(InjectionMode::Decode));
            assert_eq!(inj.should_inject(&no_hex), None);
        }
        // mode=noneならrate>0でも注入しない (復帰)
        inj.set(InjectionMode::None, 0.5);
        assert_eq!(inj.should_inject(&yes_hex), None);
    }

    #[cfg(any(debug_assertions, feature = "devtools"))]
    #[test]
    fn decode_injection_fails_job_individually_without_cache_file() {
        let dir = TestDir::new("inject-decode");
        let stats = Arc::new(ThumbStats::new());
        let cache =
            Arc::new(ThumbnailCache::open(dir.path().join("cache"), Arc::clone(&stats)).unwrap());
        let inj = Arc::new(FailureInjection::new());
        inj.set(InjectionMode::Decode, 1.0);
        let png = write_test_png(&dir, "a.png", 64, 64, false);
        let fp = fingerprint_of(&png, 10, 0);

        // 注入されたジョブ: 個別failed。キャッシュにも一時ファイルにも残さない
        let bad = ThumbnailJob {
            source: png.clone(),
            fingerprint: fp.clone(),
            size_class: SizeClass::S256,
            cache: Arc::clone(&cache),
            stats: Arc::clone(&stats),
            injection: Arc::clone(&inj),
        };
        let err = bad.run(&CancelToken::new()).unwrap_err();
        assert!(err.contains("decode failed"), "{err}");
        let key = crate::cache::cache_key(&fp, SizeClass::S256);
        assert!(matches!(
            cache.lookup(&key),
            crate::cache::CacheLookup::Miss
        ));

        // 同一プール設定で注入なしジョブは成功する (キュー全体は停止しない)
        inj.set(InjectionMode::None, 0.0);
        let good = ThumbnailJob {
            source: png,
            fingerprint: fp.clone(),
            size_class: SizeClass::S256,
            cache: Arc::clone(&cache),
            stats: Arc::clone(&stats),
            injection: Arc::clone(&inj),
        };
        assert!(good.run(&CancelToken::new()).is_ok());
        assert!(matches!(
            cache.lookup(&key),
            crate::cache::CacheLookup::Hit(_)
        ));

        assert_no_tmp_files(dir.path().join("cache").as_path());
    }

    #[cfg(any(debug_assertions, feature = "devtools"))]
    #[test]
    fn cache_write_injection_fails_after_temp_creation_but_cleans_up() {
        let dir = TestDir::new("inject-cache");
        let stats = Arc::new(ThumbStats::new());
        let cache =
            Arc::new(ThumbnailCache::open(dir.path().join("cache"), Arc::clone(&stats)).unwrap());
        let inj = Arc::new(FailureInjection::new());
        inj.set(InjectionMode::CacheWrite, 1.0);
        let png = write_test_png(&dir, "c.png", 300, 200, false);
        let fp = fingerprint_of(&png, 10, 0);

        let job = ThumbnailJob {
            source: png,
            fingerprint: fp.clone(),
            size_class: SizeClass::S256,
            cache: Arc::clone(&cache),
            stats: Arc::clone(&stats),
            injection: Arc::clone(&inj),
        };
        let err = job.run(&CancelToken::new()).unwrap_err();
        assert!(err.contains("cache commit failed"), "{err}");
        assert!(err.contains("injected"), "{err}");

        // 最終ファイルなし・一時ファイル残存なし・generated会計なし
        let key = crate::cache::cache_key(&fp, SizeClass::S256);
        assert!(matches!(
            cache.lookup(&key),
            crate::cache::CacheLookup::Miss
        ));
        assert_eq!(stats.snapshot().generated, 0);
        assert_no_tmp_files(dir.path().join("cache").as_path());
    }

    /// キャッシュツリー全体に `.tmp-*` が無いことを検証する共有ヘルパ。
    #[cfg(any(debug_assertions, feature = "devtools"))]
    fn assert_no_tmp_files(cache_root: &Path) {
        let mut tmp_count = 0;
        for shard in fs::read_dir(cache_root).unwrap() {
            let shard = shard.unwrap();
            if !shard.file_type().unwrap().is_dir() {
                continue;
            }
            for file in fs::read_dir(shard.path()).unwrap() {
                let name = file.unwrap().file_name().to_string_lossy().into_owned();
                if name.starts_with(".tmp-") {
                    tmp_count += 1;
                }
            }
        }
        assert_eq!(tmp_count, 0, "injected failure must not leave temp files");
    }

    #[cfg(any(debug_assertions, feature = "devtools"))]
    #[test]
    fn injection_mode_ipc_round_trip() {
        for name in InjectionMode::IPC_NAMES {
            let mode = InjectionMode::from_ipc(name).unwrap();
            assert_eq!(mode.as_str(), name);
        }
        assert_eq!(InjectionMode::from_ipc("none"), Some(InjectionMode::None));
        assert_eq!(InjectionMode::from_ipc("network"), None);
        assert_eq!(InjectionMode::from_ipc(""), None);
    }

    /// E2Eスモーク用データセット生成 (WP-3 §7)。
    /// `GENTO_E2E_DATASET_DIR` (既定 /tmp/gento-wp3-e2e/dataset) へ
    /// 6ファイル (5形式の静止画 + アニメGIF) を書く。AVIFはblockedのため対象外。
    /// 起動方法は docs/handoff/2026-08-30-mvp-wp3.md のE2E手順参照。
    #[test]
    #[ignore = "E2E用 (cargo test -p gento-image --lib write_e2e_dataset -- --ignored)"]
    fn write_e2e_dataset() {
        let dir = std::path::PathBuf::from(
            std::env::var("GENTO_E2E_DATASET_DIR")
                .unwrap_or_else(|_| "/tmp/gento-wp3-e2e/dataset".to_string()),
        );
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(220, 140, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8])
        }));
        // ファイル名順 = カタログ順 (00=bmp, 01=animated gif, ...)
        img.save_with_format(dir.join("00-a.bmp"), image::ImageFormat::Bmp)
            .unwrap();
        {
            let mut buf = Vec::new();
            {
                use image::codecs::gif::{GifEncoder, Repeat};
                use image::{Delay, Frame};
                let mut encoder = GifEncoder::new(&mut buf);
                encoder.set_repeat(Repeat::Infinite).unwrap();
                for i in 0..3u8 {
                    let frame = image::RgbaImage::from_fn(220, 140, |x, y| {
                        let v = (x + y + u32::from(i) * 40) % 256;
                        image::Rgba([v as u8, (255 - v) as u8, 128, 255])
                    });
                    encoder
                        .encode_frame(Frame::from_parts(
                            frame,
                            0,
                            0,
                            Delay::from_numer_denom_ms(20, 1),
                        ))
                        .unwrap();
                }
            }
            fs::write(dir.join("01-anim.gif"), &buf).unwrap();
        }
        img.save_with_format(dir.join("02-a.jpg"), image::ImageFormat::Jpeg)
            .unwrap();
        img.save_with_format(dir.join("03-a.png"), image::ImageFormat::Png)
            .unwrap();
        img.save_with_format(dir.join("04-a.tiff"), image::ImageFormat::Tiff)
            .unwrap();
        {
            let rgba = image::RgbaImage::from_fn(220, 140, |x, y| {
                image::Rgba([(x % 256) as u8, (y % 256) as u8, 77, 255])
            });
            let encoded = webp::Encoder::from_rgba(rgba.as_raw(), rgba.width(), rgba.height())
                .encode_lossless();
            fs::write(dir.join("05-a.webp"), &*encoded).unwrap();
        }
        let count = fs::read_dir(&dir).unwrap().count();
        println!("e2e dataset: {count} files in {}", dir.display());
        assert_eq!(count, 6);
    }

    // ===== WebP q80 preview品質の比較資料生成 (WP-3 §6。文書化タスク) =====
    //
    // `cargo test -p gento-image --lib preview_quality_material -- --ignored --nocapture`
    // で実行。sample/datasetから数種を読み取り (読み取り専用)、preview2048
    // (WebP q80) とoriginalを /tmp/gento-preview-quality/ へ並べて出力し、
    // サイズ比と簡易fidelity指標をstdoutへ出す。判断は
    // docs/dev/preview-quality-note.md に記録する。

    /// 元画像を同寸法へ**可逆リサイズ**した基準との平均絶対誤差 (0-255)。
    /// q80の視覚品質の定量的代理指標 (PSNR等の厳密計算は意図的に省略。
    /// 目視判断の補助)。
    fn mean_abs_diff(a: &image::RgbaImage, b: &image::RgbaImage) -> f64 {
        assert_eq!((a.width(), a.height()), (b.width(), b.height()));
        let mut total = 0u64;
        let mut count = 0u64;
        for (pa, pb) in a.pixels().zip(b.pixels()) {
            for ch in 0..3 {
                total += u64::from(pa[ch].abs_diff(pb[ch]));
                count += 1;
            }
        }
        total as f64 / count.max(1) as f64
    }

    #[test]
    #[ignore = "比較資料生成専用 (通常のテスト実行には含めない。sample/dataset依存)"]
    fn preview_quality_material() {
        // cargo testはcrate dirをcwdにするためmanifest基準で解決する
        let dataset = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sample/dataset");
        let mut sources: Vec<PathBuf> = fs::read_dir(dataset)
            .expect("sample/datasetが見つからない (repo rootで実行すること)")
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("png")))
            .collect();
        sources.sort();
        assert!(!sources.is_empty(), "datasetが空");

        // 多様性のため最初・中央・最後 + 中間の4点 (決定的選択)
        let mid = sources.len() / 2;
        let picks = [
            sources[0].clone(),
            sources[mid].clone(),
            sources[sources.len() * 3 / 4].clone(),
            sources[sources.len() - 1].clone(),
        ];

        let out_root = std::env::temp_dir().join("gento-preview-quality");
        let _ = fs::remove_dir_all(&out_root);
        fs::create_dir_all(&out_root).unwrap();

        println!(
            "=== preview品質比較資料 (出力先: {}) ===",
            out_root.display()
        );
        println!(
            "{:<44} {:>9} {:>9} {:>7} {:>8}",
            "file", "orig[B]", "prev[B]", "ratio", "meanAbs"
        );
        for (i, src) in picks.iter().enumerate() {
            let name = src.file_name().unwrap().to_string_lossy().into_owned();
            let Ok(output) = generate(src, SizeClass::Preview2048, &CancelToken::new()) else {
                println!("{name:<44} (生成失敗 — スキップ)");
                continue;
            };
            // originalのコピー (jpg埋め込み等は無いPNG前提。そのままコピー)
            let orig_out = out_root.join(format!("{i:02}-original-{name}"));
            fs::copy(src, &orig_out).unwrap();
            let prev_out = out_root.join(format!("{i:02}-preview-{name}.webp"));
            fs::write(&prev_out, &output.bytes).unwrap();

            // 基準: 元を同一寸法へ可逆縮小 (Lanczos3) したもの
            let original = image::open(src).unwrap();
            let resized_reference = if original.width() > original.height() {
                if original.width() > 2048 {
                    original.resize_exact(
                        output.width,
                        output.height,
                        image::imageops::FilterType::Lanczos3,
                    )
                } else {
                    original
                }
            } else if original.height() > 2048 {
                original.resize_exact(
                    output.width,
                    output.height,
                    image::imageops::FilterType::Lanczos3,
                )
            } else {
                original
            };
            let preview_decoded =
                image::load_from_memory_with_format(&output.bytes, image::ImageFormat::WebP)
                    .unwrap()
                    .to_rgba8();
            let reference = resized_reference.to_rgba8();
            // 寸法が食い違う場合 (切り上げ丸め) は基準を合わせる
            let reference = if (reference.width(), reference.height())
                != (preview_decoded.width(), preview_decoded.height())
            {
                image::DynamicImage::ImageRgba8(reference)
                    .resize_exact(
                        preview_decoded.width(),
                        preview_decoded.height(),
                        image::imageops::FilterType::Lanczos3,
                    )
                    .to_rgba8()
            } else {
                reference
            };
            let mad = mean_abs_diff(&preview_decoded, &reference);
            let ratio = output.bytes.len() as f64 / fs::metadata(src).unwrap().len() as f64;
            println!(
                "{:<44} {:>9} {:>9} {:>6.1}% {:>8.2}",
                name,
                fs::metadata(src).unwrap().len(),
                output.bytes.len(),
                ratio * 100.0,
                mad
            );
            let _ = prev_out;
        }
        println!("=== 判断は docs/dev/preview-quality-note.md に記録 ===");
    }
}
