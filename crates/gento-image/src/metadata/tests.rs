//! metadataモジュールのfd束ね統合テスト。

use super::test_support as ts;
use super::*;
use std::io::Write;

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "gento-meta-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ===== 元画像バイト列不変 (受け入れ条件1) =====

#[test]
fn metadata_extraction_does_not_modify_file_bytes() {
    // PNG (tEXt + eXIf) とJPEG (APP1 + APP13) の両方で、解析前後の
    // バイト列とBLAKE3が一致することを保証 (読み取り専用の証明)。
    let dir = temp_dir("immutable");
    for (name, bytes) in [
        ("a.png", ts::png_with_text_and_exif()),
        ("b.jpg", ts::jpeg_with_exif_and_iptc()),
    ] {
        let path = dir.join(name);
        std::fs::write(&path, &bytes).unwrap();
        let before = std::fs::read(&path).unwrap();
        let mut hasher = blake3::Hasher::new();
        hasher.update(&before);
        let hash_before = format!("blake3:{}", hasher.finalize().to_hex());

        let expect = stat_token_of(&path).unwrap();
        let result = hash_probe_metadata_file(&path, &expect, &AtomicBool::new(false));
        assert!(result.is_ok(), "{name}: {result:?}");
        assert!(
            !result.unwrap().metadata.is_empty(),
            "{name}: メタデータ取得済み"
        );

        let after = std::fs::read(&path).unwrap();
        assert_eq!(before, after, "{name}: バイト列不変");
        let mut hasher = blake3::Hasher::new();
        hasher.update(&after);
        assert_eq!(
            hash_before,
            format!("blake3:{}", hasher.finalize().to_hex()),
            "{name}: BLAKE3不変"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ===== トークン規律 (WP-2パターン踏襲) =====

#[test]
fn token_mismatch_is_stale_and_metadata_discarded() {
    let dir = temp_dir("stale");
    let path = dir.join("a.png");
    std::fs::write(&path, ts::png_with_text_and_exif()).unwrap();
    let mut expect = stat_token_of(&path).unwrap();
    expect.mtime_ns += 1; // 同一ms内のns差でも破棄
    let err = hash_probe_metadata_file(&path, &expect, &AtomicBool::new(false)).unwrap_err();
    assert!(matches!(err, HashFileError::Stale(_)), "{err:?}");

    // size差も同様
    let mut expect = stat_token_of(&path).unwrap();
    expect.byte_size += 1;
    assert!(matches!(
        hash_probe_metadata_file(&path, &expect, &AtomicBool::new(false)),
        Err(HashFileError::Stale(_))
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn non_image_yields_no_metadata() {
    let dir = temp_dir("nonimage");
    let path = dir.join("a.txt");
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(b"just text").unwrap();
    drop(f);
    let expect = stat_token_of(&path).unwrap();
    let result = hash_probe_metadata_file(&path, &expect, &AtomicBool::new(false)).unwrap();
    assert!(result.probe.is_none());
    assert!(result.metadata.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cancel_aborts() {
    let dir = temp_dir("cancel");
    let path = dir.join("a.png");
    std::fs::write(&path, ts::png_with_text_and_exif()).unwrap();
    let expect = stat_token_of(&path).unwrap();
    let err = hash_probe_metadata_file(&path, &expect, &AtomicBool::new(true)).unwrap_err();
    assert_eq!(err, HashFileError::Cancelled);
    let _ = std::fs::remove_dir_all(&dir);
}

// ===== 形式ディスパッチ =====

#[test]
fn gif_and_bmp_yield_no_metadata() {
    // 未対応形式 (GIF/BMP/AVIF) はparserなし = 空Vec (raw保持も無し)。
    let dir = temp_dir("gif");
    // 最小GIFシグネチャ + 論理スクリーン記述子 (プローブがGIFと判定する程度)
    let mut gif = Vec::new();
    gif.extend_from_slice(b"GIF89a");
    gif.extend_from_slice(&[16, 0, 16, 0, 0x80, 0, 0]);
    gif.extend_from_slice(&[0x21, 0xF9, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00]); // GCE
    gif.extend_from_slice(&[0x2C, 0, 0, 0, 0, 16, 0, 16, 0, 0x00]); // 画像記述子
    gif.extend_from_slice(&[0x02, 0x02, 0x44, 0x01, 0x00]); // LZW最小ブロック
    gif.extend_from_slice(&[0x3B]); // trailer
    let path = dir.join("a.gif");
    std::fs::write(&path, &gif).unwrap();
    let expect = stat_token_of(&path).unwrap();
    let result = hash_probe_metadata_file(&path, &expect, &AtomicBool::new(false)).unwrap();
    // プローブがGIFと判定できればメタデータ空、できなくても (Corrupt扱い)
    // メタデータ空 — どちらにせよ空であることを検証
    assert!(result.metadata.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
