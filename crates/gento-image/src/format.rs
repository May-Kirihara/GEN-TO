//! MVP 7形式の形式判定・寸法・アニメーション情報プローブ (MVP WP-3)。
//!
//! - 判定は**magic (先頭バイト)** が正本。拡張子は列挙 (scan) とフォールバック
//!   にのみ使う — 拡張子詐称ファイルはmagicどおりに扱われる
//! - 寸法は `image` crate の各デコーダのヘッダパース (`dimensions`) を利用。
//!   デコード割り当て上限と寸法上限を適用する (decompression bomb対策)
//! - アニメーション情報 (GIFフレーム数・WebP ANIM/ANMF) は軽量なバイト走査で
//!   取得する — フレーム取得デコードは行わない (一覧は静止画のため不要)
//! - fd束ね契約 (FG-WP2-2): [`probe_image_at`] は開いたfdの先頭から読み、
//!   戻り時にも読み取り位置を先頭へ巻き戻す (ハッシュとの束ね用)

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use gento_core::library::{ProbedFormat, ProbedImage};
use image::ImageDecoder as _;

/// プローブ (ヘッダパース) に許すデコード割り当て上限 (バイト)。
/// ヘッダ情報のみに必要な量より十分大きい安全弁 (probe.rsのPNG判定と同値)。
const PROBE_LIMIT_BYTES: u64 = 1024 * 1024;
/// png crateのLimits型向け (`bytes` はusize)。
const PROBE_LIMIT_BYTES_USIZE: usize = 1024 * 1024;

/// GIFフレーム数走査の読み取り上限 (バイト)。巨大GIFの全ファイル走査から
/// 一時走査スレッドを守る。上限到達時はそこまでのカウント (下限値) を返す。
const GIF_SCAN_MAX_BYTES: u64 = 32 * 1024 * 1024;

/// プローブ結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageProbe {
    Ok(ProbedImage),
    /// オープン不能等、ファイルとして読めない。
    Unreadable,
    /// 対象形式のmagicだが内容が不正 (切り詰め・ゴミ・寸法上限超過)。
    Corrupt,
    /// 既知の画像magicなし (拡張子は対象でも内容が未知の形式)。
    Unsupported,
}

/// `image` crateの形式識別子への対応表 (gento-coreはimageに依存しないため、
/// 対応はここでのみ持つ)。
pub fn image_format_of(format: ProbedFormat) -> image::ImageFormat {
    match format {
        ProbedFormat::Png => image::ImageFormat::Png,
        ProbedFormat::Jpeg => image::ImageFormat::Jpeg,
        ProbedFormat::Webp => image::ImageFormat::WebP,
        ProbedFormat::Gif => image::ImageFormat::Gif,
        ProbedFormat::Bmp => image::ImageFormat::Bmp,
        ProbedFormat::Tiff => image::ImageFormat::Tiff,
        ProbedFormat::Avif => image::ImageFormat::Avif,
    }
}

/// 先頭バイトから形式を判定する。判定に必要なmagic長は12バイト
/// (ftypブランドがoffset 8..12に来るため)。
pub fn sniff_format(head: &[u8]) -> Option<ProbedFormat> {
    if head.len() < 12 {
        return None;
    }
    if head.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some(ProbedFormat::Png)
    } else if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(ProbedFormat::Jpeg)
    } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        Some(ProbedFormat::Gif)
    } else if head.starts_with(b"BM") {
        Some(ProbedFormat::Bmp)
    } else if head.starts_with(&[0x49, 0x49, 0x2A, 0x00])
        || head.starts_with(&[0x4D, 0x4D, 0x00, 0x2A])
    {
        Some(ProbedFormat::Tiff)
    } else if head.starts_with(b"RIFF") && &head[8..12] == b"WEBP" {
        Some(ProbedFormat::Webp)
    } else if &head[4..8] == b"ftyp" {
        avif_brand(head)
    } else {
        None
    }
}

/// ftypブランドからAVIF判定 (avif/avis。mif1/msf1は互換ブランドで判定)。
/// HEIC等の非対象ブランドはNone (Unsupported扱い)。
fn avif_brand(head: &[u8]) -> Option<ProbedFormat> {
    let major = &head[8..12];
    if major == b"avif" || major == b"avis" {
        return Some(ProbedFormat::Avif);
    }
    if (major == b"mif1" || major == b"msf1")
        && head.windows(4).any(|w| w == b"avif" || w == b"avis")
    {
        return Some(ProbedFormat::Avif);
    }
    None
}

/// AVIFがアニメーション (シーケンス) か。ブランドベースの簡易判定。
fn avif_is_animated(head: &[u8]) -> bool {
    head.windows(4).any(|w| w == b"avis")
}

/// 既知magicの前置辞。12バイト未満の入力がこれらと (部分一致・完全一致の
/// いずれかで) 一致するなら「既知形式の切り詰め」(Corrupt) として扱う。
const MAGIC_PREFIXES: [&[u8]; 8] = [
    &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A],
    &[0xFF, 0xD8, 0xFF],
    b"GIF87a",
    b"GIF89a",
    b"BM",
    b"RIFF",
    &[0x49, 0x49, 0x2A, 0x00],
    &[0x4D, 0x4D, 0x00, 0x2A],
];

/// 短い入力が既知magicの切り詰め (magicの部分一致、またはmagic検出に
/// 必要な残りバイトの欠落) か。完全なmagic検出に12バイトを要する形式
/// (RIFF+WEBP / ftypブランド) の短いファイルもここで拾う。
fn is_truncated_magic(head: &[u8]) -> bool {
    if head.len() >= 12 {
        return false; // sniff_formatが完全判定済みの長さ
    }
    if MAGIC_PREFIXES.iter().any(|magic| {
        head.len() < magic.len() && magic.starts_with(head)
            || head.starts_with(&magic[..head.len().min(magic.len())])
    }) {
        return true;
    }
    // ftypはoffset 4..8に現れる (8バイト以上で判別可能)
    head.len() >= 8 && &head[4..8] == b"ftyp"
}

/// パスからプローブする。
pub fn probe_image(path: &Path) -> ImageProbe {
    let Ok(mut file) = File::open(path) else {
        return ImageProbe::Unreadable;
    };
    probe_image_at(&mut file)
}

/// 開いたfdからプローブする。読み取り位置は開始時と戻り時ともに先頭
/// (fd束ね契約。ハッシュ計算後に呼ぶことを想定)。
pub fn probe_image_at(file: &mut File) -> ImageProbe {
    if file.rewind().is_err() {
        return ImageProbe::Unreadable;
    }
    // magic判定をデコーダ構築の前にやる (拡張子詐称と未知形式の区別のため)。
    let mut head = [0u8; 64];
    let read = match read_at_most(file, &mut head) {
        Ok(n) => n,
        Err(_) => return ImageProbe::Unreadable,
    };
    let Some(format) = sniff_format(&head[..read]) else {
        // 空・既知magicの切り詰めはCorrupt、それ以外は未知形式 (Unsupported)
        let result = if read == 0 || is_truncated_magic(&head[..read]) {
            ImageProbe::Corrupt
        } else {
            ImageProbe::Unsupported
        };
        return rewind_and(file, result);
    };
    let animated_brand = format == ProbedFormat::Avif && avif_is_animated(&head[..read]);

    let (width, height, has_alpha) = match decode_header(file, format) {
        Ok(info) => info,
        Err(result) => return rewind_and(file, result),
    };
    // BMP/WebPのset_limitsは寸法を検査しないため、ここで明示的に上限を課す
    // (PNG/JPEG/TIFF/GIFは二重検査になるが害はない)。
    if width == 0
        || height == 0
        || width > crate::thumbnail::MAX_DECODE_DIMENSION
        || height > crate::thumbnail::MAX_DECODE_DIMENSION
    {
        return rewind_and(file, ImageProbe::Corrupt);
    }

    let (is_animated, frame_count) = match format {
        ProbedFormat::Gif => {
            let Ok(frames) = gif_frame_count(file) else {
                return rewind_and(file, ImageProbe::Corrupt);
            };
            (frames > 1, Some(frames))
        }
        ProbedFormat::Webp => {
            let Ok((has_anim, frames)) = webp_animation_info(file) else {
                return rewind_and(file, ImageProbe::Corrupt);
            };
            (has_anim, frames)
        }
        ProbedFormat::Avif => (animated_brand, None),
        _ => (false, None),
    };

    let result = ImageProbe::Ok(ProbedImage {
        format,
        width,
        height,
        has_alpha,
        is_animated,
        frame_count,
    });
    let _ = file.rewind();
    result
}

/// デコーダを都度作って寸法とカラータイプだけ得る。借用をここに閉じ込める。
fn decode_header(file: &mut File, format: ProbedFormat) -> Result<(u32, u32, bool), ImageProbe> {
    if file.rewind().is_err() {
        return Err(ImageProbe::Unreadable);
    }
    if format == ProbedFormat::Png {
        // PNGはpng crateのヘッダ読み取りのみで寸法・カラータイプが得られる
        // (IDAT不在でもヘッダ情報は正当。probe.rsの旧来動作を維持)。
        let mut decoder = png::Decoder::new_with_limits(
            BufReader::new(&mut *file),
            png::Limits {
                bytes: PROBE_LIMIT_BYTES_USIZE,
            },
        );
        return match decoder.read_header_info() {
            Ok(info) => Ok((
                info.width,
                info.height,
                matches!(
                    info.color_type,
                    png::ColorType::Rgba | png::ColorType::GrayscaleAlpha
                ),
            )),
            Err(err) => Err(match err {
                png::DecodingError::IoError(io) if io.kind() == io::ErrorKind::UnexpectedEof => {
                    ImageProbe::Corrupt
                }
                png::DecodingError::IoError(_) => ImageProbe::Unreadable,
                _ => ImageProbe::Corrupt,
            }),
        };
    }
    let mut reader =
        image::ImageReader::with_format(BufReader::new(&mut *file), image_format_of(format));
    reader.limits(probe_limits());
    match reader.into_decoder() {
        Ok(decoder) => {
            let (width, height) = decoder.dimensions();
            let has_alpha = decoder.color_type().has_alpha();
            Ok((width, height, has_alpha))
        }
        Err(err) => Err(classify_decoder_error(err)),
    }
}

/// 戻り値の前にfd位置を先頭へ戻す (呼び出し元とのfd束ね契約)。
fn rewind_and(file: &mut File, result: ImageProbe) -> ImageProbe {
    let _ = file.rewind();
    result
}

/// EOFは成功 (短いファイル) として扱う読み取り。
fn read_at_most(file: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

/// プローブ用のデコード制限。
fn probe_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(crate::thumbnail::MAX_DECODE_DIMENSION);
    limits.max_image_height = Some(crate::thumbnail::MAX_DECODE_DIMENSION);
    limits.max_alloc = Some(PROBE_LIMIT_BYTES);
    limits
}

/// デコーダ構築エラーの分類。ヘッダパース失敗は内容の問題 (Corrupt) 扱い。
/// open済みfdのIO失敗は実務上稀なため、Unreadableはopen/read_at_most
/// 失敗時に限る。
fn classify_decoder_error(err: image::ImageError) -> ImageProbe {
    use image::ImageError;
    match err {
        // 機能無効 (例: AVIFのavif-native不在) — 形式としては未知扱い
        ImageError::Unsupported(_) => ImageProbe::Unsupported,
        ImageError::Limits(_) => ImageProbe::Corrupt,
        _ => ImageProbe::Corrupt,
    }
}

/// GIFのブロック構造を走査してフレーム数 (Image Descriptor 0x2Cの数) を数える。
/// LZWデータや拡張ブロックの中身は読み飛ばす (ヘッダ走査のみ)。
/// 読み取りバイト数上限 ([`GIF_SCAN_MAX_BYTES`]) に到達した場合はそこまでの
/// カウント (下限値) を返す。
fn gif_frame_count(file: &mut File) -> io::Result<u32> {
    file.rewind()?;
    let mut reader = io::BufReader::new(&mut *file);
    let mut budget = GIF_SCAN_MAX_BYTES;

    let mut header = [0u8; 6];
    reader.read_exact(&mut header)?;
    budget = budget.saturating_sub(6);
    if &header[..3] != b"GIF" {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not a gif"));
    }
    let mut lsd = [0u8; 7];
    reader.read_exact(&mut lsd)?;
    budget = budget.saturating_sub(7);
    if lsd[4] & 0x80 != 0 {
        let gct = (3 * (1usize << ((lsd[4] & 0x07) + 1))) as u64;
        budget = skip_bytes(&mut reader, gct, budget)?;
    }

    let mut frames: u32 = 0;
    loop {
        if budget == 0 {
            return Ok(frames);
        }
        let block = read_u8(&mut reader, &mut budget)?;
        match block {
            0x21 => {
                // 拡張: ラベル + サブブロック列
                let _label = read_u8(&mut reader, &mut budget)?;
                loop {
                    let size = read_u8(&mut reader, &mut budget)?;
                    if size == 0 {
                        break;
                    }
                    budget = skip_bytes(&mut reader, u64::from(size), budget)?;
                    if budget == 0 {
                        return Ok(frames);
                    }
                }
            }
            0x2C => {
                // イメージディスクリプタ: 固定9バイト + ローカルカラーテーブル
                let mut desc = [0u8; 9];
                reader.read_exact(&mut desc)?;
                budget = budget
                    .checked_sub(9)
                    .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "budget"))?;
                if desc[8] & 0x80 != 0 {
                    let lct = (3 * (1usize << ((desc[8] & 0x07) + 1))) as u64;
                    budget = skip_bytes(&mut reader, lct, budget)?;
                }
                // ディスクリプタが読めた時点でフレームと数える (データ途中の
                // 予算打ち切りでもこのフレームはカウント済みにする)
                frames = frames.saturating_add(1);
                let _lzw_min = read_u8(&mut reader, &mut budget)?;
                loop {
                    let size = read_u8(&mut reader, &mut budget)?;
                    if size == 0 {
                        break;
                    }
                    budget = skip_bytes(&mut reader, u64::from(size), budget)?;
                    if budget == 0 {
                        return Ok(frames);
                    }
                }
            }
            0x3B => return Ok(frames),
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unexpected gif block {other:#04x}"),
                ));
            }
        }
    }
}

/// 予算内の分だけ読み飛ばす。残り予算を返す。
fn skip_bytes<R: Read>(reader: &mut io::BufReader<R>, bytes: u64, budget: u64) -> io::Result<u64> {
    let take = bytes.min(budget);
    io::copy(&mut reader.by_ref().take(take), &mut io::sink())?;
    Ok(budget - take)
}

/// 予算管理付き1バイト読み取り。
fn read_u8<R: Read>(reader: &mut io::BufReader<R>, budget: &mut u64) -> io::Result<u8> {
    if *budget == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "scan budget exhausted",
        ));
    }
    let mut buf = [0u8; 1];
    reader.read_exact(&mut buf)?;
    *budget -= 1;
    Ok(buf[0])
}

/// WebP (RIFF) のチャンクヘッダを走査し、(ANIMチャンク存在, ANMFフレーム数)
/// を返す。チャンク本体はseekで読み飛ばすため読み取り量は僅少。
fn webp_animation_info(file: &mut File) -> io::Result<(bool, Option<u32>)> {
    file.rewind()?;
    let mut riff = [0u8; 12];
    file.read_exact(&mut riff)?;
    if &riff[0..4] != b"RIFF" || &riff[8..12] != b"WEBP" {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not a webp"));
    }
    let riff_size = u64::from(u32::from_le_bytes([riff[4], riff[5], riff[6], riff[7]]));
    // RIFFサイズはファイル長と乖離し得る (破損)。走査範囲の上限として使う。
    let end = 8u64.saturating_add(riff_size);

    let mut has_anim = false;
    let mut frames = 0u32;
    let mut pos = 12u64;
    while pos + 8 <= end {
        file.seek(SeekFrom::Start(pos))?;
        let mut chunk = [0u8; 8];
        file.read_exact(&mut chunk)?;
        let size = u64::from(u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]));
        match &chunk[0..4] {
            b"ANIM" => has_anim = true,
            b"ANMF" => frames = frames.saturating_add(1),
            _ => {}
        }
        // チャンクは偶数境界にパディングされる。折り返し・飽和は打ち切り。
        let Some(next) = pos
            .checked_add(8)
            .and_then(|p| p.checked_add(size + (size & 1)))
        else {
            break;
        };
        if next <= pos {
            break;
        }
        pos = next;
    }
    Ok((has_anim, Some(frames)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// 実AVIF fixture (生成コマンド・出所は crates/gento-image/fixtures/README.md)。
    /// still-yuv420: 400x260 不透過 (ffmpeg libaom-av1)。
    const AVIF_STILL: &[u8] = include_bytes!("../fixtures/still-yuv420.avif");
    /// still-alpha: 80x60 左上4分の1透過 (ImageMagick/libheif。alpha aux item入り)。
    const AVIF_STILL_ALPHA: &[u8] = include_bytes!("../fixtures/still-alpha.avif");
    /// animated-avis: 96x64 3フレーム (ffmpeg。ブランドavis)。
    const AVIF_ANIMATED: &[u8] = include_bytes!("../fixtures/animated-avis.avif");

    struct TestFile(std::path::PathBuf);

    impl TestFile {
        fn new(name: &str, bytes: &[u8]) -> Self {
            let path = std::env::temp_dir().join(format!(
                "gento-format-{}-{}-{}",
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

    /// NフレームのGIFバイト列を生成する (image crateのエンコーダ)。
    /// fixturesの生成手段は crates/gento-image/fixtures/README.md 参照。
    fn gif_bytes(w: u32, h: u32, frames: usize) -> Vec<u8> {
        use image::codecs::gif::{GifEncoder, Repeat};
        use image::{Delay, Frame};
        assert!(frames >= 1);
        let base = image::RgbaImage::from_fn(w, h, |x, y| {
            image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255])
        });
        let framed: Vec<Frame> = (0..frames)
            .map(|i| {
                let mut buf = base.clone();
                if i > 0 {
                    for px in buf.pixels_mut() {
                        px[0] = px[0].wrapping_add(i as u8 * 37);
                    }
                }
                Frame::from_parts(buf, 0, 0, Delay::from_numer_denom_ms(20, 1))
            })
            .collect();
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut encoder = GifEncoder::new(&mut cursor);
            encoder.set_repeat(Repeat::Infinite).unwrap();
            encoder.encode_frames(framed).expect("gif encode");
        }
        cursor.into_inner()
    }

    /// ANIM/ANMFチャンクを持つ手作りWebP (チャンク走査の検証用。フレーム
    /// 本体はダミーでimageデコーダは通らない)。
    fn animated_webp_skeleton() -> Vec<u8> {
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
        let mut body = Vec::new();
        body.extend_from_slice(&chunk(b"VP8X", &[0u8; 10]));
        body.extend_from_slice(&chunk(b"ANIM", &[0u8; 6]));
        body.extend_from_slice(&chunk(b"ANMF", &[0u8; 16]));
        body.extend_from_slice(&chunk(b"ANMF", &[0u8; 16]));
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&((body.len() + 4) as u32).to_le_bytes());
        out.extend_from_slice(b"WEBP");
        out.extend_from_slice(&body);
        out
    }

    /// 実VP8LフレームをANMFで包んだ**デコード可能な**animated WebP
    /// (image-webpが先頭フレームをデコードできる。VP8XのANIMATION_FLAG
    /// 0x02が必須 — 立てないとChunkMissingになる)。
    fn animated_webp_real() -> Vec<u8> {
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
        fn le24(v: u32) -> [u8; 3] {
            [
                (v & 0xFF) as u8,
                ((v >> 8) & 0xFF) as u8,
                ((v >> 16) & 0xFF) as u8,
            ]
        }
        const W: u32 = 24;
        const H: u32 = 12;
        // 実lossless WebPからVP8Lチャンクを取り出す
        let rgba = image::RgbaImage::from_fn(W, H, |x, y| {
            image::Rgba([(x * 10 % 256) as u8, (y * 20 % 256) as u8, 77, 255])
        });
        let encoded =
            webp::Encoder::from_rgba(rgba.as_raw(), rgba.width(), rgba.height()).encode_lossless();
        let bytes: &[u8] = &encoded;
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WEBP");
        let mut pos = 12usize;
        let mut vp8l = Vec::new();
        while pos + 8 <= bytes.len() {
            let size = u32::from_le_bytes([
                bytes[pos + 4],
                bytes[pos + 5],
                bytes[pos + 6],
                bytes[pos + 7],
            ]) as usize;
            if &bytes[pos..pos + 4] == b"VP8L" {
                // RIFFチャンクは偶数境界にパディングされるため取り出しも
                // パディング込みで行う (奇数長のVP8LをANMFへ埋め直すため)
                vp8l = bytes[pos..pos + 8 + size + (size & 1)].to_vec();
            }
            pos += 8 + size + (size & 1);
        }
        assert!(!vp8l.is_empty(), "VP8Lチャンクが見つからない");

        let mut vp8x = Vec::new();
        vp8x.extend_from_slice(&[0x02u8, 0, 0, 0]); // ANIMATION_FLAG
        vp8x.extend_from_slice(&le24(W - 1));
        vp8x.extend_from_slice(&le24(H - 1));
        let anim = vec![0u8; 6]; // bg color + loop count
        let mut anmf_payload = Vec::new();
        anmf_payload.extend_from_slice(&le24(0));
        anmf_payload.extend_from_slice(&le24(0));
        anmf_payload.extend_from_slice(&le24(W - 1));
        anmf_payload.extend_from_slice(&le24(H - 1));
        anmf_payload.extend_from_slice(&le24(40)); // duration
        anmf_payload.push(0); // flags
        anmf_payload.extend_from_slice(&vp8l);

        let mut body = Vec::new();
        body.extend_from_slice(&chunk(b"VP8X", &vp8x));
        body.extend_from_slice(&chunk(b"ANIM", &anim));
        body.extend_from_slice(&chunk(b"ANMF", &anmf_payload));
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&((body.len() + 4) as u32).to_le_bytes());
        out.extend_from_slice(b"WEBP");
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn magic_detection_table() {
        let cases: Vec<(Vec<u8>, ProbedFormat)> = vec![
            (crate::probe::minimal_png_bytes(2, 2), ProbedFormat::Png),
            (
                vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 16, b'J', b'F', b'I', b'F', 0, 0],
                ProbedFormat::Jpeg,
            ),
            (
                b"GIF89a\x01\x00\x01\x00\x00\x00".to_vec(),
                ProbedFormat::Gif,
            ),
            (
                b"GIF87a\x01\x00\x01\x00\x00\x00".to_vec(),
                ProbedFormat::Gif,
            ),
            (
                vec![b'B', b'M', 0x36, 0, 0, 0, 0, 0, 0, 0, 0x36, 0, 0, 0],
                ProbedFormat::Bmp,
            ),
            (
                vec![0x49, 0x49, 0x2A, 0x00, 8, 0, 0, 0, 1, 0, 0, 0],
                ProbedFormat::Tiff,
            ),
            (
                vec![0x4D, 0x4D, 0x00, 0x2A, 0, 8, 0, 0, 1, 0, 0, 0],
                ProbedFormat::Tiff,
            ),
            (b"RIFF\x24\x00\x00\x00WEBPVP8 ".to_vec(), ProbedFormat::Webp),
            (
                vec![
                    0, 0, 0, 0x18, b'f', b't', b'y', b'p', b'a', b'v', b'i', b'f',
                ],
                ProbedFormat::Avif,
            ),
            (
                vec![
                    0, 0, 0, 0x18, b'f', b't', b'y', b'p', b'a', b'v', b'i', b's',
                ],
                ProbedFormat::Avif,
            ),
            (
                vec![
                    0, 0, 0, 0x20, b'f', b't', b'y', b'p', b'm', b'i', b'f', b'1', 0, 0, 0, 0,
                    b'a', b'v', b'i', b'f',
                ],
                ProbedFormat::Avif,
            ),
        ];
        for (bytes, want) in cases {
            assert_eq!(
                sniff_format(&bytes),
                Some(want),
                "magic: {:02x?}",
                &bytes[..bytes.len().min(12)]
            );
        }
        // 非対象: HEICブランド・テキスト・短すぎる入力
        assert_eq!(
            sniff_format(&[0, 0, 0, 0x18, b'f', b't', b'y', b'p', b'h', b'e', b'i', b'c']),
            None
        );
        assert_eq!(sniff_format(b"hello world!"), None);
        assert_eq!(sniff_format(b"RIFF"), None);
    }

    #[test]
    fn probe_each_supported_format_returns_dims() {
        let dir = std::env::temp_dir().join(format!("gento-format-fmt-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mut checked = 0;

        let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(32, 20, |x, y| {
            image::Rgba([x as u8, y as u8, 99, 255])
        }));

        let png_path = dir.join("a.png");
        img.save(&png_path).unwrap();
        match probe_image(&png_path) {
            ImageProbe::Ok(p) => {
                assert_eq!((p.width, p.height), (32, 20));
                assert_eq!(p.format, ProbedFormat::Png);
                assert!(p.has_alpha);
                assert!(!p.is_animated);
                assert_eq!(p.frame_count, None);
                checked += 1;
            }
            other => panic!("png: {other:?}"),
        }

        let jpeg_path = dir.join("a.jpg");
        image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(48, 24, |x, y| {
            image::Rgb([x as u8, y as u8, 7])
        }))
        .save(&jpeg_path)
        .unwrap();
        match probe_image(&jpeg_path) {
            ImageProbe::Ok(p) => {
                assert_eq!((p.width, p.height), (48, 24));
                assert_eq!(p.format, ProbedFormat::Jpeg);
                assert!(!p.has_alpha, "RGB JPEGにalphaはない");
                checked += 1;
            }
            other => panic!("jpeg: {other:?}"),
        }

        let bmp_path = dir.join("a.bmp");
        img.save(&bmp_path).unwrap();
        match probe_image(&bmp_path) {
            ImageProbe::Ok(p) => {
                assert_eq!((p.width, p.height), (32, 20));
                assert_eq!(p.format, ProbedFormat::Bmp);
                checked += 1;
            }
            other => panic!("bmp: {other:?}"),
        }

        let gif_path = dir.join("a.gif");
        fs::write(&gif_path, gif_bytes(24, 16, 1)).unwrap();
        match probe_image(&gif_path) {
            ImageProbe::Ok(p) => {
                assert_eq!((p.width, p.height), (24, 16));
                assert_eq!(p.format, ProbedFormat::Gif);
                assert!(!p.is_animated, "1フレームGIFは非アニメ");
                assert_eq!(p.frame_count, Some(1));
                checked += 1;
            }
            other => panic!("gif: {other:?}"),
        }

        let webp_path = dir.join("a.webp");
        let rgba = image::RgbaImage::from_fn(40, 30, |x, y| {
            image::Rgba([x as u8, y as u8, 5, if x < 10 { 0 } else { 255 }])
        });
        let encoded =
            webp::Encoder::from_rgba(rgba.as_raw(), rgba.width(), rgba.height()).encode_lossless();
        fs::write(&webp_path, &*encoded).unwrap();
        match probe_image(&webp_path) {
            ImageProbe::Ok(p) => {
                assert_eq!((p.width, p.height), (40, 30));
                assert_eq!(p.format, ProbedFormat::Webp);
                assert!(p.has_alpha);
                assert!(!p.is_animated);
                assert_eq!(p.frame_count, Some(0));
                checked += 1;
            }
            other => panic!("webp: {other:?}"),
        }

        let tiff_path = dir.join("a.tiff");
        img.save(&tiff_path).unwrap();
        match probe_image(&tiff_path) {
            ImageProbe::Ok(p) => {
                assert_eq!((p.width, p.height), (32, 20));
                assert_eq!(p.format, ProbedFormat::Tiff);
                checked += 1;
            }
            other => panic!("tiff: {other:?}"),
        }

        // AVIF (実fixture。avif-native有効時にdav1dでプローブ成功)
        let avif_path = dir.join("a.avif");
        fs::write(&avif_path, AVIF_STILL).unwrap();
        match probe_image(&avif_path) {
            ImageProbe::Ok(p) => {
                assert_eq!((p.width, p.height), (400, 260));
                assert_eq!(p.format, ProbedFormat::Avif);
                // image crateのAvifDecoderはalpha planeの有無にかかわらず
                // color_type=Rgba8を返すため、AVIFのhas_alphaは常にtrueに
                // なる (保守的な過大検出。decode時のalphaは実payloadどおり)
                assert!(p.has_alpha);
                assert!(!p.is_animated);
                assert_eq!(p.frame_count, None);
                checked += 1;
            }
            other => panic!("avif: {other:?}"),
        }

        assert_eq!(checked, 7, "7形式すべてプローブ成功であること");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn animated_gif_reports_frames_and_animation() {
        let f = TestFile::new("anim-gif", &gif_bytes(12, 8, 3));
        match probe_image(&f.0) {
            ImageProbe::Ok(p) => {
                assert_eq!(p.format, ProbedFormat::Gif);
                assert!(p.is_animated);
                assert_eq!(p.frame_count, Some(3));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn real_animated_webp_probes_and_decodes_first_frame() {
        let bytes = animated_webp_real();
        let f = TestFile::new("anim-webp-real", &bytes);
        // プローブ: アニメーション検出 (ANIM + ANMF 1フレーム)
        match probe_image(&f.0) {
            ImageProbe::Ok(p) => {
                assert_eq!(p.format, ProbedFormat::Webp);
                assert_eq!((p.width, p.height), (24, 12));
                assert!(p.is_animated, "ANIMチャンク存在 = animated");
                assert_eq!(p.frame_count, Some(1));
            }
            other => panic!("probe: {other:?}"),
        }
        // デコード: 先頭フレームが静止画として得られる (サムネイル経路)
        let decoded =
            image::ImageReader::with_format(std::io::Cursor::new(&bytes), image::ImageFormat::WebP)
                .decode()
                .expect("image-webpはanimated webpの先頭フレームをデコードできる");
        assert_eq!((decoded.width(), decoded.height()), (24, 12));
    }

    #[test]
    fn webp_chunk_walk_detects_anim_and_frames() {
        let f = TestFile::new("anim-webp", &animated_webp_skeleton());
        let mut file = File::open(&f.0).unwrap();
        let (has_anim, frames) = webp_animation_info(&mut file).expect("チャンク走査は成功する");
        assert!(has_anim);
        assert_eq!(frames, Some(2));
    }

    /// alpha aux itemを持つ実AVIF: プローブはhas_alpha=true、デコード結果の
    /// RGBAは透過領域を維持する (dav1d + mp4parseのalpha併合経路)。
    #[test]
    fn avif_alpha_item_probes_and_keeps_transparency() {
        let f = TestFile::new("avif-alpha", AVIF_STILL_ALPHA);
        match probe_image(&f.0) {
            ImageProbe::Ok(p) => {
                assert_eq!(p.format, ProbedFormat::Avif);
                assert_eq!((p.width, p.height), (80, 60));
                assert!(p.has_alpha);
                assert!(!p.is_animated);
            }
            other => panic!("probe: {other:?}"),
        }
        let decoded = crate::thumbnail::decode_with_limits(&f.0).expect("decode");
        let rgba = decoded.to_rgba8();
        // 左上4分の1は透過、右下は不透明 (alphaは損失圧縮のため閾値判定)
        let tl = rgba.get_pixel(2, 2).0[3];
        let br = rgba.get_pixel(70, 50).0[3];
        assert!(tl <= 32, "transparent corner alpha={tl}");
        assert!(br >= 224, "opaque corner alpha={br}");
    }

    /// ブランドavis (アニメーションAVIF): プローブは is_animated=true を返し、
    /// デコードは先頭フレームを静止画として得る (他アニメ形式と同一の規約)。
    #[test]
    fn animated_avis_avif_probes_animation_and_decodes_first_frame() {
        let f = TestFile::new("avif-anim", AVIF_ANIMATED);
        match probe_image(&f.0) {
            ImageProbe::Ok(p) => {
                assert_eq!(p.format, ProbedFormat::Avif);
                assert_eq!((p.width, p.height), (96, 64));
                assert!(p.is_animated, "avisブランド = animated");
                // AVIFはフレーム数走査を行わない (既知の簡易判定)
                assert_eq!(p.frame_count, None);
            }
            other => panic!("probe: {other:?}"),
        }
        let decoded = crate::thumbnail::decode_with_limits(&f.0).expect("先頭フレームのデコード");
        assert_eq!((decoded.width(), decoded.height()), (96, 64));
    }

    #[test]
    fn truncated_and_garbage_inputs_are_safe() {
        // テキストファイル (拡張子はpngだがmagicなし) → Unsupported
        let f = TestFile::new("text", b"this is a text file, not an image");
        assert_eq!(probe_image(&f.0), ImageProbe::Unsupported);

        // 空ファイル → Corrupt (旧PNGプローブの挙動と同一)
        let f = TestFile::new("empty", b"");
        assert_eq!(probe_image(&f.0), ImageProbe::Corrupt);

        // PNGシグネチャのみ → Corrupt
        let f = TestFile::new("trunc-png", &crate::probe::minimal_png_bytes(4, 4)[..10]);
        assert_eq!(probe_image(&f.0), ImageProbe::Corrupt);

        // AVIFの切り詰め: ftyp途中 (10バイト) はmagic切り詰めとしてCorrupt、
        // コンテナ途中 (先頭100バイト) はmp4parse/dav1dの失敗としてCorrupt
        let f = TestFile::new("trunc-avif-magic", &AVIF_STILL[..10]);
        assert_eq!(probe_image(&f.0), ImageProbe::Corrupt);
        let f = TestFile::new("trunc-avif-body", &AVIF_STILL[..100]);
        assert_eq!(probe_image(&f.0), ImageProbe::Corrupt);

        // 存在しない → Unreadable
        assert_eq!(
            probe_image(Path::new("/nonexistent-gento/x.png")),
            ImageProbe::Unreadable
        );
    }

    #[test]
    fn dimension_limit_rejects_bomb_headers() {
        // 100000x100000を主張するPNG (CRC込みの正当なIHDR)
        let png = crate::probe::minimal_png_bytes(100_000, 100_000);
        let f = TestFile::new("bomb-png", &png);
        assert_eq!(probe_image(&f.0), ImageProbe::Corrupt);

        // BMP: 100000x100000を主張するヘッダ (BITMAPINFOHEADER)
        let mut bmp = Vec::new();
        bmp.extend_from_slice(b"BM");
        bmp.extend_from_slice(&[0u8; 12]); // filesize/reserved/offset (ダミー)
        bmp.extend_from_slice(&40u32.to_le_bytes()); // ヘッダサイズ
        bmp.extend_from_slice(&100_000i32.to_le_bytes()); // 幅
        bmp.extend_from_slice(&100_000i32.to_le_bytes()); // 高さ
        bmp.extend_from_slice(&1u16.to_le_bytes()); // プレーン数
        bmp.extend_from_slice(&24u16.to_le_bytes()); // bpp
        bmp.extend_from_slice(&[0u8; 24]);
        let f = TestFile::new("bomb-bmp", &bmp);
        assert_eq!(probe_image(&f.0), ImageProbe::Corrupt);
    }

    /// 固定シードの簡易LCG (依存追加なしの変異源)。
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
    fn random_mutations_never_panic() {
        // 各形式のシードデータを固定シードLCGで変異させ、プローブが
        // panicしないことを確認する (Ok/Corrupt/Unsupportedはどれも許容)。
        let jpeg_seed = {
            let mut cursor = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(16, 16, |x, y| {
                image::Rgb([x as u8, y as u8, 3])
            }))
            .write_to(&mut cursor, image::ImageFormat::Jpeg)
            .unwrap();
            cursor.into_inner()
        };
        let seeds: Vec<Vec<u8>> = vec![
            crate::probe::minimal_png_bytes(8, 8),
            gif_bytes(8, 8, 1),
            animated_webp_skeleton(),
            jpeg_seed,
            AVIF_STILL.to_vec(),
            AVIF_STILL_ALPHA.to_vec(),
        ];
        let mut rng = Lcg(0x5EED_1234_ABCD_0001);
        for (si, seed) in seeds.iter().enumerate() {
            for i in 0..64u32 {
                let mut bytes = seed.clone();
                let flips = (rng.next() % 4) + 1;
                for _ in 0..flips {
                    let pos = (rng.next() as usize) % bytes.len();
                    bytes[pos] ^= ((rng.next() % 255) + 1) as u8;
                }
                let f = TestFile::new(&format!("mut-{si}-{i}"), &bytes);
                let result = std::panic::catch_unwind(|| probe_image(&f.0));
                assert!(result.is_ok(), "mutation must not panic");
            }
        }
    }

    #[test]
    fn probe_image_at_rewinds_and_repeats() {
        // fd束ね契約: 2回連続プローブが同一結果を返す (位置が巻き戻る)。
        let f = TestFile::new("twice", &gif_bytes(6, 6, 2));
        let mut file = File::open(&f.0).unwrap();
        let first = probe_image_at(&mut file);
        let second = probe_image_at(&mut file);
        assert_eq!(first, second);
        assert!(matches!(first, ImageProbe::Ok(_)));
    }
}
