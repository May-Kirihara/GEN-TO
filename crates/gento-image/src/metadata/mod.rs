//! 埋め込み画像メタデータの解析 (MVP WP-9)。
//!
//! - **EXIF** (`exif@1`): JPEG APP1 / TIFF / PNG eXIf / WebP EXIF。
//!   解析は kamadak-exif (`Reader`)。コンテナ抽出はJPEG/PNGが自前走査
//!   (上限付き)、TIFF/WebPはcrateの `read_from_container` (読み取り総量を
//!   [ReadLimiter] で上限)。主要項目 (撮影日時・カメラ・レンズ・GPS・
//!   Orientation) を正規化し、全フィールドを `raw_exif` JSONへ。
//! - **IPTC IIM** (`iptc@1`): JPEG APP13 8BIM内のIIMレコードを最小自作解析。
//!   keywords/caption/byline/copyright等の一般的レコードを `raw_iptc` JSONへ
//!   (正規化列は持たない — 写真系列の正規化はEXIFが正本)。
//! - **PNGテキストチャンク** (`png-text@1`): tEXt/iTXt/zTXt全収集
//!   (1チャンク4MiB・合計16MiB上限。超過は警告付きスキップ)。
//!   `raw_png_metadata` JSONへ。
//! - **SD WebUI/Forge parameters** (`sd-params@1`): `parameters`チャンクの
//!   テキスト形式 (prompt / `Negative prompt:` 行 / `Steps: …, …` カンマ
//!   区切りkey:value)。
//! - **ComfyUI** (`comfy@1`): `prompt` JSONノードグラフからKSampler系の
//!   positive/negative入力に接続されたCLIPTextEncodeテキストを接続追跡で
//!   抽出 (ベストエフォート)。`workflow` JSONは `raw_workflow` へverbatim
//!   保持のみ。
//! - **NovelAI** (`nai@1`): `Comment` (JSON) / `Title` / `Description` /
//!   `Software` / `Source` チャンク。
//!
//! すべて**読み取り専用**。元ファイルのバイト列は一切変更しない
//! (spec-draft §12.1。呼び出し元のfd束ねフローでBLAKE3前後一致を保証)。

pub mod exif;
pub mod gen;
pub mod iptc;
pub mod png_chunks;

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use gento_core::assetmeta::AssetMetadataParsed;
use gento_core::library::{FileContentHash, GenerationToken, HashFileError, HashedWithMetadata};

use crate::hash::fd_token;
use crate::scan::{os_file_id_of, system_time_to_ns};

/// parser名 (asset_metadata.parser_name)。
pub const PARSER_EXIF: &str = "exif";
pub const PARSER_IPTC: &str = "iptc";
pub const PARSER_PNG_TEXT: &str = "png-text";
pub const PARSER_SD_PARAMS: &str = "sd-params";
pub const PARSER_COMFY: &str = "comfy";
pub const PARSER_NAI: &str = "nai";

/// 現行parser集合 (名前, version)。再解析判定の正本。
/// - 出力形式を変えたparserは該当versionを繰り上げる (既存行の再解析対象化)。
/// - **parserの新規追加・削除は全parserのversionを繰り上げる**:
///   形式ごとに生成される行は異なるため「行の欠落」では新parserを必要と
///   するassetを特定できない。全version繰り上げで全assetを再解析対象に
///   する (冪等スキップにより実際の書き換えは変化した行のみ)。
pub const CURRENT_PARSERS: &[(&str, u32)] = &[
    (PARSER_EXIF, 1),
    (PARSER_IPTC, 1),
    (PARSER_PNG_TEXT, 1),
    (PARSER_SD_PARAMS, 1),
    (PARSER_COMFY, 1),
    (PARSER_NAI, 1),
];

/// ハッシュ計算の読み取りチャンクサイズ (hash.rsと同じ)。
const CHUNK_BYTES: usize = 1024 * 1024;

/// 1つの開いたfdからBLAKE3ハッシュ+形式プローブ+埋め込みメタデータ解析を
/// 束ねて計算する (WP-9。`hash_and_probe_file` の拡張)。
///
/// - fdトークン ≠ `expect` → 計算前に [`HashFileError::Stale`] (無駄読みなし)
/// - BLAKE3計算 (1MiBチャンク・協調キャンセル) → 同一fdからプローブ →
///   同一fdからメタデータ抽出
/// - 全読み取り後にfdトークン不変を検証 (in-place変更検出)。変化はStale
///   (メタデータ結果も含めて破棄 — 別世代のメタデータ混入の構造的排除)。
pub fn hash_probe_metadata_file(
    path: &Path,
    expect: &GenerationToken,
    cancel: &AtomicBool,
) -> Result<HashedWithMetadata, HashFileError> {
    let mut file = File::open(path).map_err(|e| HashFileError::Io(e.to_string()))?;
    let before = fd_token(&file)?;
    if before != *expect {
        return Err(HashFileError::Stale(format!(
            "{}: fd token {:?} differs from scan observation {:?} before hashing",
            path.display(),
            before,
            expect
        )));
    }
    let byte_size = before.byte_size;
    let mtime_ns = before.mtime_ns;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; CHUNK_BYTES];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(HashFileError::Cancelled);
        }
        let read = file
            .read(&mut buf)
            .map_err(|e| HashFileError::Io(e.to_string()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    let probe = match crate::format::probe_image_at(&mut file) {
        crate::format::ImageProbe::Ok(info) => Some(info),
        crate::format::ImageProbe::Unreadable
        | crate::format::ImageProbe::Corrupt
        | crate::format::ImageProbe::Unsupported => None,
    };
    let metadata = extract_metadata_at(&mut file, probe, cancel);
    let after = fd_token(&file)?;
    if after != before {
        return Err(HashFileError::Stale(format!(
            "{}: fd token changed during hashing ({:?} -> {:?})",
            path.display(),
            before,
            after
        )));
    }
    Ok(HashedWithMetadata {
        content: FileContentHash {
            content_hash: format!("blake3:{}", hasher.finalize().to_hex()),
            byte_size,
            mtime_ms: mtime_ns / 1_000_000,
            mtime_ns,
            os_file_id: before.os_file_id,
        },
        probe,
        metadata,
    })
}

/// 再解析 (`metadata_reparse` 用): ハッシュは計算済み (assetは内容アドレス
/// 済み) のため、開いたfd上でトークン照合→形式プローブ→メタデータ抽出→
/// 再照合のみ行う。トークン不一致は [`HashFileError::Stale`] — 呼び出し側は
/// そのassetをスキップする (次回スキャンで再ハッシュされる)。
pub fn reparse_metadata_file(
    path: &Path,
    expect: &GenerationToken,
) -> Result<Vec<AssetMetadataParsed>, HashFileError> {
    let mut file = File::open(path).map_err(|e| HashFileError::Io(e.to_string()))?;
    let before = fd_token(&file)?;
    if before != *expect {
        return Err(HashFileError::Stale(format!(
            "{}: fd token {:?} differs from record {:?} before reparse",
            path.display(),
            before,
            expect
        )));
    }
    let probe = match crate::format::probe_image_at(&mut file) {
        crate::format::ImageProbe::Ok(info) => Some(info),
        crate::format::ImageProbe::Unreadable
        | crate::format::ImageProbe::Corrupt
        | crate::format::ImageProbe::Unsupported => None,
    };
    let metadata = extract_metadata_at(&mut file, probe, &AtomicBool::new(false));
    let after = fd_token(&file)?;
    if after != before {
        return Err(HashFileError::Stale(format!(
            "{}: fd token changed during reparse ({:?} -> {:?})",
            path.display(),
            before,
            after
        )));
    }
    Ok(metadata)
}

/// 開いたfd (位置は問わない — 内部でrewindする) から形式に応じたparser群を
/// 適用する。非画像・未対応形式・読み取り失敗は空Vec
/// (メタデータなし = 保存も行わない)。
pub fn extract_metadata_at(
    file: &mut File,
    probe: Option<gento_core::library::ProbedImage>,
    cancel: &AtomicBool,
) -> Vec<AssetMetadataParsed> {
    use gento_core::library::ProbedFormat;
    let Some(image) = probe else {
        return Vec::new();
    };
    match image.format {
        ProbedFormat::Png => {
            let Some(chunks) = png_chunks::collect_png_texts(file, cancel) else {
                return Vec::new();
            };
            let mut out = Vec::new();
            if let Some(row) = exif::parse_exif_tiff_bytes(chunks.exif.as_deref()) {
                out.push(row);
            }
            out.extend(gen::parse_generation_chunks(&chunks));
            out.push(png_chunks::png_text_row(&chunks));
            out
        }
        ProbedFormat::Jpeg => {
            let segments = jpeg_segments(file, cancel);
            let mut out = Vec::new();
            if let Some(row) = exif::parse_exif_tiff_bytes(segments.exif_payload.as_deref()) {
                out.push(row);
            }
            if let Some(row) = iptc::parse_iptc_app13(&segments.app13_payloads) {
                out.push(row);
            }
            out
        }
        ProbedFormat::Tiff | ProbedFormat::Webp => {
            // TIFF/WebPはEXIFのみ (コンテナ抽出はkamadak-exifに一任。
            // 読み取り総量はReadLimiterで制限)。
            match exif::parse_exif_container(file) {
                Some(row) => vec![row],
                None => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

/// JPEG APPセグメント走査結果 (EXIF/IPTCに必要な分のみ)。
struct JpegSegments {
    exif_payload: Option<Vec<u8>>,
    app13_payloads: Vec<Vec<u8>>,
}

/// JPEGセグメントを走査し、APP1 (Exif) のTIFF本体とAPP13のペイロード群を
/// 取り出す。セグメント長はu16で本質的に64KiB以下。総読み取り量とセグメント
/// 数に上限を設ける (異常ファイルへの安全弁)。
fn jpeg_segments(file: &mut File, cancel: &AtomicBool) -> JpegSegments {
    use std::io::{Read, Seek, SeekFrom};
    let mut out = JpegSegments {
        exif_payload: None,
        app13_payloads: Vec::new(),
    };
    if file.rewind().is_err() {
        return out;
    }
    let mut header = [0u8; 2];
    if file.read_exact(&mut header).is_err() || header != [0xFF, 0xD8] {
        return out; // SOIなし (プローブと判定が割れた異常ケース)
    }
    const MAX_SEGMENTS: usize = 4096;
    const MAX_TOTAL: usize = 64 * 1024 * 1024;
    let mut total = 0usize;
    let mut marker = [0u8; 2];
    for _ in 0..MAX_SEGMENTS {
        if cancel.load(Ordering::Relaxed) {
            return out;
        }
        if file.read_exact(&mut marker).is_err() {
            return out;
        }
        if marker[0] != 0xFF {
            return out; // セグメント境界が崩れたら以降は信頼しない
        }
        if marker[1] == 0xFF {
            continue; // 填充 (padding)
        }
        let kind = marker[1];
        // EOI (0xD9)・RST (0xD0..=0xD7)・TEM (0x01) は長さを持たない
        if kind == 0xD9 || (0xD0..=0xD7).contains(&kind) || kind == 0x01 {
            continue;
        }
        let mut len_be = [0u8; 2];
        if file.read_exact(&mut len_be).is_err() {
            return out;
        }
        let seg_len = u16::from_be_bytes(len_be) as usize;
        if seg_len < 2 {
            return out;
        }
        let payload_len = seg_len - 2;
        total = total.saturating_add(payload_len);
        if total > MAX_TOTAL {
            return out;
        }
        let is_exif = kind == 0xE1 && out.exif_payload.is_none();
        let is_iptc = kind == 0xED;
        if !is_exif && !is_iptc {
            if file.seek(SeekFrom::Current(payload_len as i64)).is_err() {
                return out;
            }
            if kind == 0xDA {
                return out; // SOS以降は圧縮データ — 走査終了
            }
            continue;
        }
        let mut payload = vec![0u8; payload_len];
        if file.read_exact(&mut payload).is_err() {
            return out;
        }
        if is_exif {
            // "Exif\0\0" ヘッダを剥がす (無い場合はそのまま = 生TIFF)
            let tiff = payload
                .strip_prefix(b"Exif\0\0")
                .map(<[u8]>::to_vec)
                .unwrap_or(payload);
            out.exif_payload = Some(tiff);
        } else {
            out.app13_payloads.push(payload);
        }
    }
    out
}

/// 読み取り総量上限 (EXIFコンテナ抽出の安全弁。exif.rsのLimitedSeekable)。
pub(crate) const METADATA_READ_LIMIT_BYTES: u64 = 64 * 1024 * 1024;

/// 安定ファイルの期待トークンを構成する (hash.rsのstat_token相当)。
pub fn stat_token_of(path: &Path) -> Option<GenerationToken> {
    let md = std::fs::metadata(path).ok()?;
    Some(GenerationToken {
        os_file_id: os_file_id_of(&md),
        byte_size: md.len(),
        mtime_ns: system_time_to_ns(md.modified()),
    })
}

#[cfg(test)]
mod tests;

/// テスト共通のfixture生成器 (出所: すべてRustコード上で手組み生成。
/// 外部ツール・実ファイル依存なし — 決定的に再現可能)。
#[cfg(test)]
pub(crate) mod test_support;
