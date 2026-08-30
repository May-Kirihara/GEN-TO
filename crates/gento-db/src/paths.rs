//! パスの符号化 (MVP WP-1。PM計画§6-3)。
//!
//! 相対パス・絶対パスとも、DBには生バイト列BLOB + 符号化タグ
//! (`path_encoding`: `'unix-bytes'` | `'utf8'`) で保持し、表示名は
//! lossy変換したTEXTで別に持つ。これによりLinuxの非UTF-8パスを
//! 失わない。
//!
//! - Unix: `OsStr` の生バイトをそのまま格納する。UTF-8として正当な
//!   場合は `utf8`、不正バイトを含む場合は `unix-bytes` を付す。
//! - Windows等: WP-1ではUTF-8表現のみ (lossy)。WTF-8/UTF-16完全保持は
//!   WP-17検証時に再検討する (PM計画§6-3)。
//!
//! backup/restoreの正準表現はパーセントエスケープUTF-8文字列
//! ([`percent_encode_bytes`] / [`percent_decode_to_bytes`])。
//! 非ASCIIも含めたバイト完全性を保ちつつ、ASCIIパスは可読なまま。

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// `path_encoding` 列の値。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PathEncoding {
    /// パスはUTF-8として正当。
    Utf8,
    /// Unix生バイト列 (UTF-8として不正なバイトを含む)。
    UnixBytes,
}

impl PathEncoding {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            PathEncoding::Utf8 => "utf8",
            PathEncoding::UnixBytes => "unix-bytes",
        }
    }

    /// DB格納値から復元。不正値は `None`。
    pub(crate) fn from_db_value(s: &str) -> Option<Self> {
        match s {
            "utf8" => Some(PathEncoding::Utf8),
            "unix-bytes" => Some(PathEncoding::UnixBytes),
            _ => None,
        }
    }
}

/// パスを (生バイト, 符号化タグ) へ変換する。
pub(crate) fn encode_path(path: &Path) -> (Vec<u8>, PathEncoding) {
    #[cfg(unix)]
    {
        let bytes = std::os::unix::ffi::OsStrExt::as_bytes(path.as_os_str());
        if path.to_str().is_some() {
            (bytes.to_vec(), PathEncoding::Utf8)
        } else {
            (bytes.to_vec(), PathEncoding::UnixBytes)
        }
    }
    #[cfg(not(unix))]
    {
        // 非Unix: UTF-8表現のみ (WP-17でネイティブ保持を再検討)。
        (
            path.to_string_lossy().into_owned().into_bytes(),
            PathEncoding::Utf8,
        )
    }
}

/// 生バイト列をパスへ復元する。Unixでは符号化タグによらず生バイトで
/// `OsString` を再構成する (utf8タグのバイト列もUnix表現と同一のため)。
pub(crate) fn decode_path(bytes: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(OsString::from_vec(bytes.to_vec()))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
    }
}

/// backup/restore正準表現: バイト列 → パーセントエスケープ文字列。
///
/// 非予約 (英数字・`-._~/`) はそのまま、それ以外のバイトは `%XX`
/// (大文字hex) で表現する。`%` 自体は常に `%25` へエスケープされる
/// ため、復元は一意 (lossless roundtrip)。
pub fn percent_encode_bytes(bytes: &[u8]) -> String {
    const UNRESERVED: fn(u8) -> bool =
        |b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/');
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if UNRESERVED(b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// 正準表現 → バイト列。`%XX` 以外の `%` は不正として `None`。
pub fn percent_decode_to_bytes(s: &str) -> Option<Vec<u8>> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                if !bytes.get(i + 1).is_some_and(|c| c.is_ascii_hexdigit())
                    || !bytes.get(i + 2).is_some_and(|c| c.is_ascii_hexdigit())
                {
                    return None;
                }
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
                let byte = u8::from_str_radix(hex, 16).ok()?;
                out.push(byte);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use std::os::unix::ffi::OsStringExt;

    #[test]
    #[cfg(unix)]
    fn utf8_path_round_trip() {
        let p = PathBuf::from("/data/photos/画像.png");
        let (bytes, enc) = encode_path(&p);
        assert_eq!(enc, PathEncoding::Utf8);
        assert_eq!(enc.as_str(), "utf8");
        assert_eq!(decode_path(&bytes), p);
    }

    #[test]
    #[cfg(unix)]
    fn non_utf8_path_round_trip() {
        // 不正バイト (0xFF 0xFE) を含む相対パス: BLOB保存 → OsStr復元
        let os = OsString::from_vec(vec![
            b'd', b'i', b'r', b'/', 0xff, 0xfe, b'.', b'p', b'n', b'g',
        ]);
        let p = PathBuf::from(os.clone());
        assert!(p.to_str().is_none(), "前提: 非UTF-8パス");
        let (bytes, enc) = encode_path(&p);
        assert_eq!(enc, PathEncoding::UnixBytes);
        assert_eq!(enc.as_str(), "unix-bytes");
        assert_eq!(
            bytes,
            vec![b'd', b'i', b'r', b'/', 0xff, 0xfe, b'.', b'p', b'n', b'g']
        );
        let back = decode_path(&bytes);
        assert_eq!(back, p);
        assert_eq!(back.as_os_str(), os.as_os_str());
    }

    #[test]
    #[cfg(unix)]
    fn non_utf8_relative_path_via_os_str() {
        let rel = PathBuf::from(OsString::from_vec(vec![0x80, b'a', b'/', 0xc3, 0x28]));
        let (bytes, enc) = encode_path(&rel);
        assert_eq!(enc, PathEncoding::UnixBytes);
        assert_eq!(decode_path(&bytes), rel);
    }

    #[test]
    fn encoding_tag_db_values() {
        assert_eq!(PathEncoding::Utf8.as_str(), "utf8");
        assert_eq!(PathEncoding::UnixBytes.as_str(), "unix-bytes");
        assert_eq!(
            PathEncoding::from_db_value("utf8"),
            Some(PathEncoding::Utf8)
        );
        assert_eq!(
            PathEncoding::from_db_value("unix-bytes"),
            Some(PathEncoding::UnixBytes)
        );
        assert_eq!(PathEncoding::from_db_value("wtf-8"), None);
    }

    #[test]
    fn percent_encode_keeps_readable_and_escapes_rest() {
        assert_eq!(percent_encode_bytes(b"a/b.png"), "a/b.png");
        assert_eq!(percent_encode_bytes(b"100%.png"), "100%25.png");
        assert_eq!(percent_encode_bytes(b"a b.png"), "a%20b.png");
        // 非ASCII (UTF-8の「画」= E7 94 BB)
        assert_eq!(percent_encode_bytes("画".as_bytes()), "%E7%94%BB");
        // 非UTF-8バイトも %XX で保持
        assert_eq!(percent_encode_bytes(&[0xff, 0xfe]), "%FF%FE");
        // 制御文字
        assert_eq!(percent_encode_bytes(&[0x00, 0x0a]), "%00%0A");
    }

    #[test]
    fn percent_decode_round_trip_lossless() {
        let cases: Vec<Vec<u8>> = vec![
            b"plain/path.png".to_vec(),
            "画像/フォルダ.png".as_bytes().to_vec(),
            vec![0xff, 0xfe, b'.', 0x00],
            b"space and % percent".to_vec(),
            Vec::new(),
        ];
        for bytes in cases {
            let enc = percent_encode_bytes(&bytes);
            let dec = percent_decode_to_bytes(&enc).expect("roundtrip decode");
            assert_eq!(dec, bytes, "enc={enc}");
        }
    }

    #[test]
    fn percent_decode_rejects_malformed() {
        assert_eq!(percent_decode_to_bytes("a%2"), None);
        assert_eq!(percent_decode_to_bytes("a%XY"), None);
        assert_eq!(percent_decode_to_bytes("%"), None);
        assert_eq!(percent_decode_to_bytes("a%G1"), None);
        // 大文字・小文字hex両方受理
        assert_eq!(percent_decode_to_bytes("%2f"), Some(vec![0x2f]));
        assert_eq!(percent_decode_to_bytes("%2F"), Some(vec![0x2f]));
    }
}
