//! ファイル内容のBLAKE3ハッシュ + 形式プローブ (MVP WP-2 / WP-3 / FG-WP2-2)。
//!
//! Asset同一性判定の正本 (data-model §2.1「バイト列ハッシュ一致 = 同一Asset」)。
//! ストリーミング (1MiBチャンク) で計算するためメモリ使用はファイルサイズに
//! 依存しない。チャンクごとにキャンセルフラグを検査する協調キャンセル。
//!
//! 世代検証 (FG-WP2-2): すべての観測は**1つの開いたfd**に束ねられる。
//! パスを再解決しないためrename/replace (inode差し替え) の影響を受けない。
//! fdのfstatトークン (os_file_id, size, mtime_ns) が走査時観測 `expect` と
//! 一致する場合のみ計算し、計算後に再びfstatしてトークン不変を検証する。
//! プローブ (magic+寸法+アニメーション情報) も同一fdから読むため、ハッシュ
//! 対象バイトと異なる情報が混入することは構造的に不可能。不一致・変化は
//! [`HashFileError::Stale`] として結果を破棄する (呼び出し側は要再提出)。

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use blake3::Hasher;
use gento_core::library::{FileContentHash, GenerationToken, HashFileError, HashedFile};

use crate::format::{probe_image_at, ImageProbe};
use crate::scan::{os_file_id_of, system_time_to_ns};

/// 読み取りチャンクサイズ。
const CHUNK_BYTES: usize = 1024 * 1024;

/// 開いたfdのfstatから世代トークンを観測する。
pub(crate) fn fd_token(file: &File) -> Result<GenerationToken, HashFileError> {
    let md = file
        .metadata()
        .map_err(|e| HashFileError::Io(e.to_string()))?;
    Ok(GenerationToken {
        os_file_id: os_file_id_of(&md),
        byte_size: md.len(),
        mtime_ns: system_time_to_ns(md.modified()),
    })
}

/// 1つの開いたfdからBLAKE3ハッシュと形式プローブを束ねて計算する。
///
/// - fdトークン ≠ `expect` → 計算前に [`HashFileError::Stale`] (無駄読みなし)
/// - BLAKE3計算 (1MiBチャンク・協調キャンセル)
/// - 同一fdから形式プローブ (magic+寸法+アニメーション) を取得
///   (非画像・破損は `None`)
/// - 計算前後でfdトークン不変を検証 (in-place変更検出)。変化はStale
///
/// 戻り値の `content_hash` は `blake3:<64hex小文字>` 形式。
pub fn hash_and_probe_file(
    path: &Path,
    expect: &GenerationToken,
    cancel: &AtomicBool,
) -> Result<HashedFile, HashFileError> {
    let mut file = File::open(path).map_err(|e| HashFileError::Io(e.to_string()))?;
    // fd経由の観測: open時点のinodeに固定される (以降のrename/replaceの
    // 影響を受けない)。
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
    // プローブは同一fdから (パス再オープン禁止 = 置換ファイルの情報混入が
    // 構造的に不可能)。ハッシュ読み取りでfd位置はEOFにあるため rewind して
    // ヘッダを読む。
    let probe = match probe_image_at(&mut file) {
        ImageProbe::Ok(info) => Some(info),
        // 非画像・読めない・壊れている: プローブNULLで登録 (寸法等はNULL)
        ImageProbe::Unreadable | ImageProbe::Corrupt | ImageProbe::Unsupported => None,
    };
    let after = fd_token(&file)?;
    if after != before {
        return Err(HashFileError::Stale(format!(
            "{}: fd token changed during hashing ({:?} -> {:?})",
            path.display(),
            before,
            after
        )));
    }
    Ok(HashedFile {
        content: FileContentHash {
            content_hash: format!("blake3:{}", hasher.finalize().to_hex()),
            byte_size,
            mtime_ms: mtime_ns / 1_000_000,
            mtime_ns,
            os_file_id: before.os_file_id,
        },
        probe,
    })
}

/// 安定ファイルの期待トークンを構成する (テスト・呼び出し側の便宜)。
pub fn stat_token(path: &Path) -> Option<GenerationToken> {
    let md = std::fs::metadata(path).ok()?;
    Some(GenerationToken {
        os_file_id: os_file_id_of(&md),
        byte_size: md.len(),
        mtime_ns: system_time_to_ns(md.modified()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::system_time_to_ms;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("gento-hash-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn known_vectors_empty_and_abc() {
        // BLAKE3公式テストベクトル (https://github.com/BLAKE3-team/BLAKE3)
        let dir = temp_dir("vectors");
        let empty = dir.join("empty.bin");
        std::fs::write(&empty, b"").unwrap();
        let hashed = hash_and_probe_file(
            &empty,
            &stat_token(&empty).unwrap(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(
            hashed.content.content_hash,
            "blake3:af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
        assert_eq!(hashed.content.byte_size, 0);
        assert_eq!(hashed.probe, None, "非画像はプローブNULL");

        let abc = dir.join("abc.bin");
        std::fs::write(&abc, b"abc").unwrap();
        let hashed =
            hash_and_probe_file(&abc, &stat_token(&abc).unwrap(), &AtomicBool::new(false)).unwrap();
        assert_eq!(
            hashed.content.content_hash,
            "blake3:6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
        );
        assert_eq!(hashed.content.byte_size, 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stable_file_hash_carries_verified_token() {
        // 安定ファイルのハッシュは (os_file_id, size, mtime_ns) の検証済み
        // 観測を伴う。この観測がリンク時の世代検証 (gento-core) の照合対象。
        let dir = temp_dir("mtime");
        let file = dir.join("a.bin");
        std::fs::write(&file, b"stable bytes").unwrap();
        let md = std::fs::metadata(&file).unwrap();
        let expect_ns = crate::scan::system_time_to_ns(md.modified());
        let expect_ms = system_time_to_ms(md.modified());
        let hashed =
            hash_and_probe_file(&file, &stat_token(&file).unwrap(), &AtomicBool::new(false))
                .unwrap();
        assert_eq!(hashed.content.byte_size, md.len());
        assert_eq!(hashed.content.mtime_ns, expect_ns);
        assert_eq!(hashed.content.mtime_ms, expect_ms);
        assert_eq!(hashed.content.os_file_id.is_some(), cfg!(unix));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn expect_token_mismatch_is_stale_before_hashing() {
        // 走査時観測とfd観測のトークン不一致は計算前にStale
        // (ms同値でもns差だけで破棄される = ms丸め問題の解消)。
        let dir = temp_dir("stale-early");
        let file = dir.join("a.bin");
        std::fs::write(&file, b"bytes").unwrap();
        let mut expect = stat_token(&file).unwrap();
        expect.mtime_ns += 1; // 同一ms内のns差
        let result = hash_and_probe_file(&file, &expect, &AtomicBool::new(false));
        assert!(
            matches!(&result, Err(HashFileError::Stale(msg)) if msg.contains("before hashing")),
            "{result:?}"
        );

        // size差も同様
        let mut expect = stat_token(&file).unwrap();
        expect.byte_size += 1;
        assert!(matches!(
            hash_and_probe_file(&file, &expect, &AtomicBool::new(false)),
            Err(HashFileError::Stale(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dims_come_from_same_fd_as_hash() {
        // プローブ情報はハッシュと同一fdから得られる (API契約: 1呼び出しで
        // hash+dimsが束ねて返る)。
        use gento_core::library::{ProbedFormat, ProbedImage};
        let dir = temp_dir("dims");
        let file = dir.join("a.png");
        std::fs::write(&file, crate::probe::minimal_png_bytes(640, 480)).unwrap();
        let hashed =
            hash_and_probe_file(&file, &stat_token(&file).unwrap(), &AtomicBool::new(false))
                .unwrap();
        assert_eq!(
            hashed.probe,
            Some(ProbedImage {
                format: ProbedFormat::Png,
                width: 640,
                height: 480,
                has_alpha: true,
                is_animated: false,
                frame_count: None,
            })
        );
        // 同一内容のインメモリ計算と一致
        let bytes = std::fs::read(&file).unwrap();
        let mut hasher = blake3::Hasher::new();
        hasher.update(&bytes);
        assert_eq!(
            hashed.content.content_hash,
            format!("blake3:{}", hasher.finalize().to_hex())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn large_file_streams_without_size_limit() {
        // チャンク境界 (1MiB) を跨ぐ内容。メモリ一定で計算できることの
        // 動作確認 (容量そのものは小さくして高速化)。
        let dir = temp_dir("big");
        let big = dir.join("big.bin");
        let pattern = vec![0xA5u8; 3 * 1024 * 1024 + 123];
        std::fs::write(&big, &pattern).unwrap();
        let hashed =
            hash_and_probe_file(&big, &stat_token(&big).unwrap(), &AtomicBool::new(false)).unwrap();
        assert_eq!(hashed.content.byte_size, pattern.len() as u64);
        // 同一内容のインメモリ計算と一致すること
        let mut hasher = blake3::Hasher::new();
        hasher.update(&pattern);
        assert_eq!(
            hashed.content.content_hash,
            format!("blake3:{}", hasher.finalize().to_hex())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancel_flag_aborts_mid_file() {
        let dir = temp_dir("cancel");
        let big = dir.join("big.bin");
        std::fs::write(&big, vec![0u8; 5 * 1024 * 1024]).unwrap();
        let cancel = AtomicBool::new(true);
        assert_eq!(
            hash_and_probe_file(&big, &stat_token(&big).unwrap(), &cancel).unwrap_err(),
            HashFileError::Cancelled
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_is_io_error() {
        let path = Path::new("/nonexistent/nope.bin");
        let err = hash_and_probe_file(path, &GenerationToken::default(), &AtomicBool::new(false))
            .unwrap_err();
        assert!(matches!(err, HashFileError::Io(_)), "{err}");
        assert!(err.to_string().contains("hash io error"));
    }
}
