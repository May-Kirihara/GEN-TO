//! PNGシグネチャ+IHDRのみを読む寸法プローブ (Phase 2 / WP-2)。
//!
//! 全文デコードは行わない (docs/implementation-plan.md §9-3)。
//! png 0.18 の `Decoder::read_header_info` を使用し、
//! `Limits` でデコード制限 (安全弁) を設定する。

use std::fs::File;
use std::io::{BufReader, Seek};
use std::path::Path;

use png::{Decoder, DecodingError, Limits};

/// ヘッダ読み取りに許すデコード割り当て上限 (バイト)。
/// IHDR読み取りには数KiBも不要だが、異常チャンクに対する安全弁として設定する。
const PROBE_LIMIT_BYTES: usize = 1024 * 1024;

/// プローブ結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeResult {
    Ok {
        width: u32,
        height: u32,
    },
    /// オープン不能等、ファイルとして読めない。
    Unreadable,
    /// PNGとして不正 (シグネチャなし・切り詰め・ゴミ)。
    Corrupt,
}

/// PNGヘッダ (IHDR) から幅・高さを取得する。
pub fn probe_png_dimensions(path: &Path) -> ProbeResult {
    let file = match File::open(path) {
        Ok(f) => f,
        // 権限不足・不存在等は「読めない」
        Err(_) => return ProbeResult::Unreadable,
    };
    probe_png_dimensions_at(&file)
}

/// 開いたfdからPNGヘッダ (IHDR) を読んで幅・高さを取得する (FG-WP2-2)。
///
/// ハッシュ計算に使った**同一fd**へのプローブ束ねに使用する — パスを
/// 再オープンしないため、ハッシュ対象バイトと異なる置換ファイルの寸法が
/// 混入することは構造的に不可能。読み取り位置は先頭に巻き戻す (fd自体は
/// 呼び出し元と共有。ハッシュ完了後の呼び出しを想定し位置干渉はない)。
pub fn probe_png_dimensions_at(file: &File) -> ProbeResult {
    let mut file = file;
    if file.rewind().is_err() {
        return ProbeResult::Unreadable;
    }
    let mut decoder = Decoder::new_with_limits(
        BufReader::new(&mut file),
        Limits {
            bytes: PROBE_LIMIT_BYTES,
        },
    );
    match decoder.read_header_info() {
        Ok(info) => {
            if info.width == 0 || info.height == 0 {
                return ProbeResult::Corrupt;
            }
            ProbeResult::Ok {
                width: info.width,
                height: info.height,
            }
        }
        Err(err) => match err {
            // 切り詰めファイルはEOF到達になるが、内容の問題なのでCorrupt扱い
            DecodingError::IoError(io) if io.kind() == std::io::ErrorKind::UnexpectedEof => {
                ProbeResult::Corrupt
            }
            DecodingError::IoError(_) => ProbeResult::Unreadable,
            _ => ProbeResult::Corrupt,
        },
    }
}

/// テスト用最小PNG (シグネチャ + IHDR + IEND)。CRC32はチャンク検証される
/// ため正しく計算する。fs.rs等の他テストモジュールからも使用。
#[cfg(test)]
pub(crate) fn minimal_png_bytes(width: u32, height: u32) -> Vec<u8> {
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
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8bit RGBA
    png.extend_from_slice(&chunk(b"IHDR", &ihdr));
    png.extend_from_slice(&chunk(b"IEND", &[]));
    png
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use super::minimal_png_bytes as make_png_bytes;

    struct TestFile(std::path::PathBuf);

    impl TestFile {
        fn new(name: &str, bytes: &[u8]) -> Self {
            let path = std::env::temp_dir().join(format!(
                "gento-probe-{}-{}-{}",
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

    #[test]
    fn valid_png_returns_dimensions() {
        let f = TestFile::new("ok", &make_png_bytes(3, 2));
        assert_eq!(
            probe_png_dimensions(&f.0),
            ProbeResult::Ok {
                width: 3,
                height: 2
            }
        );

        let big = TestFile::new("big", &make_png_bytes(3840, 2048));
        assert_eq!(
            probe_png_dimensions(&big.0),
            ProbeResult::Ok {
                width: 3840,
                height: 2048
            }
        );
    }

    #[test]
    fn truncated_png_is_corrupt() {
        let full = make_png_bytes(10, 10);
        // シグネチャのみ
        let f = TestFile::new("trunc-sig", &full[..8]);
        assert_eq!(probe_png_dimensions(&f.0), ProbeResult::Corrupt);

        // IHDR途中で切り詰め
        let f = TestFile::new("trunc-ihdr", &full[..16]);
        assert_eq!(probe_png_dimensions(&f.0), ProbeResult::Corrupt);
    }

    #[test]
    fn garbage_without_signature_is_corrupt() {
        let f = TestFile::new("garbage", b"this is a text file, not a png at all");
        assert_eq!(probe_png_dimensions(&f.0), ProbeResult::Corrupt);
    }

    #[test]
    fn empty_file_is_corrupt() {
        let f = TestFile::new("empty", b"");
        assert_eq!(probe_png_dimensions(&f.0), ProbeResult::Corrupt);
    }

    #[test]
    fn bad_ihdr_crc_is_corrupt() {
        let mut bytes = make_png_bytes(10, 10);
        // IHDRチャンクのCRC (sig 8 + len 4 + type 4 + data 13 の直後4バイト) を破壊
        let ihdr_crc_last = 8 + 4 + 4 + 13 + 3;
        bytes[ihdr_crc_last] ^= 0xFF;
        let f = TestFile::new("badcrc", &bytes);
        assert_eq!(probe_png_dimensions(&f.0), ProbeResult::Corrupt);
    }

    #[test]
    fn missing_file_is_unreadable() {
        let p = std::env::temp_dir().join("gento-probe-definitely-missing.png");
        assert_eq!(probe_png_dimensions(&p), ProbeResult::Unreadable);
    }

    #[test]
    fn fd_probe_matches_path_probe() {
        // FG-WP2-2: fd経由プローブはパス経由と同一結果を返す
        // (hash_and_probeからの束ね呼び出しの土台)。
        let ok = TestFile::new("fd-ok", &make_png_bytes(12, 34));
        let file = File::open(&ok.0).unwrap();
        assert_eq!(
            probe_png_dimensions_at(&file),
            ProbeResult::Ok {
                width: 12,
                height: 34
            }
        );

        let garbage = TestFile::new("fd-garbage", b"not a png at all");
        let file = File::open(&garbage.0).unwrap();
        assert_eq!(probe_png_dimensions_at(&file), ProbeResult::Corrupt);
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_file_permissions() {
        use std::os::unix::fs::PermissionsExt;
        if fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("Uid:"))
                    .and_then(|l| l.split_whitespace().nth(1).map(|u| u == "0"))
            })
            .unwrap_or(false)
        {
            return; // rootは権限で読めなくなることがない
        }
        let f = TestFile::new("perm000", &make_png_bytes(4, 4));
        fs::set_permissions(&f.0, fs::Permissions::from_mode(0o000)).unwrap();
        assert_eq!(probe_png_dimensions(&f.0), ProbeResult::Unreadable);
        // 後片付け
        fs::set_permissions(&f.0, fs::Permissions::from_mode(0o644)).unwrap();
    }
}
