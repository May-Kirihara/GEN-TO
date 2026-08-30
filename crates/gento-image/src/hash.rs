//! ファイル内容のBLAKE3ハッシュ (MVP WP-2)。
//!
//! Asset同一性判定の正本 (data-model §2.1「バイト列ハッシュ一致 = 同一Asset」)。
//! ストリーミング (1MiBチャンク) で計算するためメモリ使用はファイルサイズに
//! 依存しない。チャンクごとにキャンセルフラグを検査する協調キャンセル。

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use blake3::Hasher;
use gento_core::library::{FileContentHash, HashFileError};

/// 読み取りチャンクサイズ。
const CHUNK_BYTES: usize = 1024 * 1024;

/// ファイル全体のBLAKE3ハッシュを計算する。
/// 戻り値の `content_hash` は `blake3:<64hex小文字>` 形式。
pub fn blake3_file_hash(
    path: &Path,
    cancel: &AtomicBool,
) -> Result<FileContentHash, HashFileError> {
    let mut file = File::open(path).map_err(|e| HashFileError::Io(e.to_string()))?;
    let byte_size = file
        .metadata()
        .map_err(|e| HashFileError::Io(e.to_string()))?
        .len();
    let mut hasher = Hasher::new();
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
    Ok(FileContentHash {
        content_hash: format!("blake3:{}", hasher.finalize().to_hex()),
        byte_size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors_empty_and_abc() {
        // BLAKE3公式テストベクトル (https://github.com/BLAKE3-team/BLAKE3)
        let dir = std::env::temp_dir().join(format!("gento-hash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let empty = dir.join("empty.bin");
        std::fs::write(&empty, b"").unwrap();
        let hash = blake3_file_hash(&empty, &AtomicBool::new(false)).unwrap();
        assert_eq!(
            hash.content_hash,
            "blake3:af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
        assert_eq!(hash.byte_size, 0);

        let abc = dir.join("abc.bin");
        std::fs::write(&abc, b"abc").unwrap();
        let hash = blake3_file_hash(&abc, &AtomicBool::new(false)).unwrap();
        assert_eq!(
            hash.content_hash,
            "blake3:6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
        );
        assert_eq!(hash.byte_size, 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn large_file_streams_without_size_limit() {
        // チャンク境界 (1MiB) を跨ぐ内容。メモリ一定で計算できることの
        // 動作確認 (容量そのものは小さくして高速化)。
        let dir = std::env::temp_dir().join(format!("gento-hash-big-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let big = dir.join("big.bin");
        let pattern = vec![0xA5u8; 3 * 1024 * 1024 + 123];
        std::fs::write(&big, &pattern).unwrap();
        let hash = blake3_file_hash(&big, &AtomicBool::new(false)).unwrap();
        assert_eq!(hash.byte_size, pattern.len() as u64);
        // 同一内容のインメモリ計算と一致すること
        let mut hasher = blake3::Hasher::new();
        hasher.update(&pattern);
        assert_eq!(
            hash.content_hash,
            format!("blake3:{}", hasher.finalize().to_hex())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancel_flag_aborts_mid_file() {
        let dir = std::env::temp_dir().join(format!("gento-hash-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let big = dir.join("big.bin");
        std::fs::write(&big, vec![0u8; 5 * 1024 * 1024]).unwrap();
        let cancel = AtomicBool::new(true);
        assert_eq!(
            blake3_file_hash(&big, &cancel).unwrap_err(),
            HashFileError::Cancelled
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_is_io_error() {
        let err = blake3_file_hash(Path::new("/nonexistent/nope.bin"), &AtomicBool::new(false))
            .unwrap_err();
        assert!(matches!(err, HashFileError::Io(_)), "{err}");
        assert!(err.to_string().contains("hash io error"));
    }
}
