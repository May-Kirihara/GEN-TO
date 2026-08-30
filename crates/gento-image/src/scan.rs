//! データセットディレクトリの走査とPNGファイル列挙 (Phase 2 / WP-2)。
//!
//! - walkdirで再帰走査。拡張子 `.png` は大小文字無視で列挙
//! - 各エントリの size / mtime を取得
//! - walkdir自体のエラー (権限等) は [`ScanEntryError`] として収集し継続
//!   (個別エラーで走査全体を停止しない、本体計画 §6)
//! - シンボリックリンクは辿らない

use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::time::Instant;

use walkdir::WalkDir;

/// sinkへ渡すバッチの最大件数。
pub const SCAN_BATCH_SIZE: usize = 64;

/// 走査で列挙した1ファイル。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedFile {
    pub path: PathBuf,
    pub size: u64,
    /// Unixエポックからのミリ秒。
    pub mtime_ms: u64,
}

/// 走査中の個別エラー (走査は継続する)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanEntryError {
    pub path: PathBuf,
    pub reason: String,
}

/// sinkへ渡るバッチと累積進捗。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanBatch {
    /// 列挙したファイルエントリの累積数 (拡張子を問わない)。
    pub entries_walked: u64,
    /// このバッチのPNGファイル。
    pub files: Vec<ScannedFile>,
}

/// 走査全体の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanResult {
    /// 列挙したファイルエントリ総数 (拡張子を問わない)。
    pub entries: u64,
    /// 見つかったPNG数。
    pub png_count: u64,
    /// 個別エラー (権限不足、metadata失敗等)。
    pub errors: Vec<ScanEntryError>,
    pub elapsed_ms: u64,
}

/// PNGファイルを再帰列挙し、バッチごとに `sink` へ渡す。
pub fn scan_pngs(root: &Path, sink: &mut dyn FnMut(ScanBatch)) -> ScanResult {
    let started = Instant::now();
    let mut entries: u64 = 0;
    let mut png_count: u64 = 0;
    let mut errors: Vec<ScanEntryError> = Vec::new();
    let mut pending: Vec<ScannedFile> = Vec::with_capacity(SCAN_BATCH_SIZE);

    for entry in WalkDir::new(root).into_iter() {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                // 権限不足等のwalkdirエラー。収集して走査を継続する。
                errors.push(ScanEntryError {
                    path: err
                        .path()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| root.to_path_buf()),
                    reason: err.to_string(),
                });
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        entries += 1;
        if !is_png_entry(entry.path()) {
            continue;
        }
        match entry.metadata() {
            Ok(md) => pending.push(scanned_file(entry.path().to_path_buf(), &md)),
            Err(err) => errors.push(ScanEntryError {
                path: entry.path().to_path_buf(),
                reason: err.to_string(),
            }),
        }
        if pending.len() >= SCAN_BATCH_SIZE {
            png_count += pending.len() as u64;
            sink(ScanBatch {
                entries_walked: entries,
                files: std::mem::take(&mut pending),
            });
        }
    }
    if !pending.is_empty() {
        png_count += pending.len() as u64;
        sink(ScanBatch {
            entries_walked: entries,
            files: pending,
        });
    }

    ScanResult {
        entries,
        png_count,
        errors,
        elapsed_ms: started.elapsed().as_millis() as u64,
    }
}

pub(crate) fn system_time_to_ms(t: std::io::Result<std::time::SystemTime>) -> u64 {
    t.map(|t| {
        t.duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    })
    .unwrap_or(0)
}

/// ns精度のmtime (FG-WP2-2世代トークン成分)。取得不能 (stat失敗・
/// エポック前) は0 — 同一環境内の比較のみに使うため一貫していればよい。
pub(crate) fn system_time_to_ns(t: std::io::Result<std::time::SystemTime>) -> u64 {
    t.map(|t| {
        t.duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos().min(u64::MAX as u128) as u64)
            .unwrap_or(0)
    })
    .unwrap_or(0)
}

pub(crate) fn is_png_entry(path: &Path) -> bool {
    path.extension()
        .map(|ext| ext.eq_ignore_ascii_case("png"))
        .unwrap_or(false)
}

/// MVP対象の画像拡張子 (計画書§11の7形式。実デコードはWP-3)。
pub const IMAGE_EXTENSIONS: [&str; 9] = [
    "png", "jpg", "jpeg", "webp", "gif", "bmp", "tif", "tiff", "avif",
];

/// 拡張子がMVP対象画像形式か (大小文字無視)。
pub fn is_image_entry(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            IMAGE_EXTENSIONS
                .iter()
                .any(|candidate| ext.eq_ignore_ascii_case(candidate))
        })
        .unwrap_or(false)
}

/// metadataからOSファイルIDを取り出す (WP-2。Linux: dev+inode)。
/// シンボリックリンクは走査対象外のため、常に実ファイルのmetadata。
#[cfg(unix)]
pub fn os_file_id_of(md: &Metadata) -> Option<gento_core::repo::OsFileId> {
    use std::os::unix::fs::MetadataExt;
    Some(gento_core::repo::OsFileId::new(md.dev(), md.ino()))
}

#[cfg(not(unix))]
pub fn os_file_id_of(_md: &Metadata) -> Option<gento_core::repo::OsFileId> {
    None
}

/// 走査で列挙した画像ファイル (WP-2。os_file_id付き)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedImage {
    pub path: PathBuf,
    pub size: u64,
    /// Unixエポックからのミリ秒。
    pub mtime_ms: u64,
    /// Unixエポックからのナノ秒 (世代トークン成分)。
    pub mtime_ns: u64,
    pub os_file_id: Option<gento_core::repo::OsFileId>,
}

/// MVP対象7形式を再帰列挙し、1ファイルごとに `sink` へ渡す (WP-2登録ルート用)。
/// rel_path解決は呼び出し側 (gento-coreのScanFs実装) で行う。
/// DB書き込みのバッチ化も呼び出し側の責務 (ここは純粋な列挙)。
pub fn scan_images(root: &Path, sink: &mut dyn FnMut(ScannedImage)) -> ScanResult {
    let started = Instant::now();
    let mut entries: u64 = 0;
    let mut image_count: u64 = 0;
    let mut errors: Vec<ScanEntryError> = Vec::new();

    for entry in WalkDir::new(root).into_iter() {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                errors.push(ScanEntryError {
                    path: err
                        .path()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| root.to_path_buf()),
                    reason: err.to_string(),
                });
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        entries += 1;
        if !is_image_entry(entry.path()) {
            continue;
        }
        match entry.metadata() {
            Ok(md) => {
                sink(ScannedImage {
                    path: entry.path().to_path_buf(),
                    size: md.len(),
                    mtime_ms: system_time_to_ms(md.modified()),
                    mtime_ns: system_time_to_ns(md.modified()),
                    os_file_id: os_file_id_of(&md),
                });
                image_count += 1;
            }
            Err(err) => errors.push(ScanEntryError {
                path: entry.path().to_path_buf(),
                reason: err.to_string(),
            }),
        }
    }

    ScanResult {
        entries,
        png_count: image_count,
        errors,
        elapsed_ms: started.elapsed().as_millis() as u64,
    }
}

pub(crate) fn scanned_file(path: PathBuf, md: &Metadata) -> ScannedFile {
    ScannedFile {
        size: md.len(),
        mtime_ms: system_time_to_ms(md.modified()),
        path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU32, Ordering};

    static DIR_SEQ: AtomicU32 = AtomicU32::new(0);

    /// テスト用一時ディレクトリ。Dropで削除する (sample/datasetには依存しない)。
    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let seq = DIR_SEQ.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "gento-image-test-{}-{}-{}",
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

        fn write(&self, rel: &str, bytes: &[u8]) -> PathBuf {
            let p = self.0.join(rel);
            fs::write(&p, bytes).unwrap();
            p
        }

        fn mkdir(&self, rel: &str) -> PathBuf {
            let p = self.0.join(rel);
            fs::create_dir_all(&p).unwrap();
            p
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// rootで実行されていると権限系テストが成立しないため、その場合はスキップ。
    #[cfg(unix)]
    fn running_as_root() -> bool {
        fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("Uid:"))
                    .and_then(|l| l.split_whitespace().nth(1).map(|u| u == "0"))
            })
            .unwrap_or(false)
    }

    const PNG_BYTES: &[u8] = b"dummy content is fine for scan (not probed here)";

    #[test]
    fn empty_directory_yields_nothing() {
        let dir = TestDir::new("empty");
        let mut batches = Vec::new();
        let result = scan_pngs(dir.path(), &mut |b| batches.push(b));
        assert!(batches.is_empty());
        assert_eq!(result.entries, 0);
        assert_eq!(result.png_count, 0);
        assert!(result.errors.is_empty());
    }

    #[test]
    fn missing_root_reports_error_and_continues() {
        let dir = TestDir::new("missing");
        let root = dir.path().join("does-not-exist");
        let mut batches = Vec::new();
        let result = scan_pngs(&root, &mut |b| batches.push(b));
        assert!(batches.is_empty());
        assert_eq!(result.png_count, 0);
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0].path, root);
        assert!(!result.errors[0].reason.is_empty());
    }

    #[test]
    fn ignores_non_png_and_matches_extension_case() {
        let dir = TestDir::new("ext");
        dir.write("a.png", PNG_BYTES);
        dir.write("b.txt", b"hello");
        dir.write("c.jpg", b"hello");
        dir.write("d.PNG", PNG_BYTES);
        dir.write("e.Png", PNG_BYTES);
        let sub = dir.mkdir("sub");
        fs::write(sub.join("nested.png"), PNG_BYTES).unwrap();

        let mut files = Vec::new();
        let result = scan_pngs(dir.path(), &mut |b| files.extend(b.files));
        let mut names: Vec<String> = files
            .iter()
            .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec!["a.png", "d.PNG", "e.Png", "nested.png"]);
        assert_eq!(result.entries, 6); // a,b,c,d,e + sub/nested.png
        assert_eq!(result.png_count, 4);
        assert!(result.errors.is_empty());
    }

    #[test]
    fn batches_are_bounded_and_complete() {
        let dir = TestDir::new("batch");
        let total = SCAN_BATCH_SIZE * 2 + 2; // 130
        for i in 0..total {
            dir.write(&format!("f{i:04}.png"), PNG_BYTES);
        }
        let mut batch_sizes = Vec::new();
        let mut files = 0usize;
        let result = scan_pngs(dir.path(), &mut |b| {
            batch_sizes.push(b.files.len());
            files += b.files.len();
        });
        assert!(batch_sizes.iter().all(|n| *n <= SCAN_BATCH_SIZE));
        assert_eq!(files, total);
        assert_eq!(result.png_count, total as u64);
        assert_eq!(result.entries, total as u64);
    }

    #[test]
    fn collects_size_mtime_and_progress() {
        let dir = TestDir::new("meta");
        dir.write("a.png", PNG_BYTES);
        let mut files = Vec::new();
        let mut progress = Vec::new();
        scan_pngs(dir.path(), &mut |b| {
            progress.push(b.entries_walked);
            files.extend(b.files);
        });
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].size, PNG_BYTES.len() as u64);
        // 実ファイルのmtimeなので「最近」であることを確認 (2023年以降)
        assert!(files[0].mtime_ms > 1_677_000_000_000);
        assert_eq!(progress, vec![1]);
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_directory_is_collected_as_error() {
        use std::os::unix::fs::PermissionsExt;
        if running_as_root() {
            // rootは chmod 000 でも読めてしまうため検証にならない
            return;
        }
        let dir = TestDir::new("perm-dir");
        let sub = dir.mkdir("locked");
        fs::write(sub.join("inside.png"), PNG_BYTES).unwrap();
        fs::set_permissions(&sub, fs::Permissions::from_mode(0o000)).unwrap();

        let mut files = Vec::new();
        let result = scan_pngs(dir.path(), &mut |b| files.extend(b.files));
        // 後片付け: Dropでの削除に失敗しないよう権限を戻す
        fs::set_permissions(&sub, fs::Permissions::from_mode(0o755)).unwrap();

        assert!(files.is_empty());
        assert!(result.png_count == 0);
        assert!(!result.errors.is_empty());
        assert!(result.errors.iter().any(|e| e
            .path
            .file_name()
            .map(|n| n == "locked")
            .unwrap_or(false)));
    }

    #[test]
    fn missing_metadata_is_collected_as_error() {
        // metadata取得失敗を直接のせる (稀少パスの擬似的検証)
        let err = ScanEntryError {
            path: PathBuf::from("/x"),
            reason: "boom".to_string(),
        };
        assert_eq!(err.path, PathBuf::from("/x"));
    }

    #[test]
    fn scan_images_covers_mvp_extensions_case_insensitively() {
        let dir = TestDir::new("imgext");
        dir.write("a.png", PNG_BYTES);
        dir.write("b.JPG", PNG_BYTES);
        dir.write("c.jpeg", PNG_BYTES);
        dir.write("d.WebP", PNG_BYTES);
        dir.write("e.gif", PNG_BYTES);
        dir.write("f.bmp", PNG_BYTES);
        dir.write("g.TIFF", PNG_BYTES);
        dir.write("h.tif", PNG_BYTES);
        dir.write("i.avif", PNG_BYTES);
        dir.write("skip.txt", b"x");
        dir.write("j.pngx", b"x");
        let sub = dir.mkdir("sub");
        fs::write(sub.join("nested.png"), PNG_BYTES).unwrap();

        let mut files = Vec::new();
        let result = scan_images(dir.path(), &mut |img| files.push(img));
        let mut names: Vec<String> = files
            .iter()
            .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "a.png",
                "b.JPG",
                "c.jpeg",
                "d.WebP",
                "e.gif",
                "f.bmp",
                "g.TIFF",
                "h.tif",
                "i.avif",
                "nested.png"
            ]
        );
        assert_eq!(result.entries, 12);
        assert_eq!(result.png_count, 10);
        assert!(result.errors.is_empty());
        // サイズ・mtimeも載る
        assert_eq!(files[0].size, PNG_BYTES.len() as u64);
        assert!(files[0].mtime_ms > 0);
    }

    #[cfg(unix)]
    #[test]
    fn scan_images_collects_os_file_id_on_unix() {
        let dir = TestDir::new("imgos");
        dir.write("a.png", PNG_BYTES);
        let mut ids = Vec::new();
        scan_images(dir.path(), &mut |img| ids.push(img.os_file_id));
        assert_eq!(ids.len(), 1);
        assert!(ids[0].is_some());
    }

    #[test]
    fn is_image_entry_cases() {
        for ok in [
            "x.png", "x.PNG", "x.jpg", "x.JpEg", "x.webp", "x.gif", "x.bmp", "x.tiff", "x.tif",
            "x.avif",
        ] {
            assert!(is_image_entry(Path::new(&format!("/a/{ok}"))), "{ok}");
        }
        for ng in ["x.pngx", "x.txt", "png", "x.PNG ", ".png"] {
            assert!(!is_image_entry(Path::new(&format!("/a/{ng}"))), "{ng}");
        }
    }

    #[test]
    fn is_png_entry_cases() {
        assert!(is_png_entry(Path::new("/a/b.png")));
        assert!(is_png_entry(Path::new("/a/b.PNG")));
        assert!(is_png_entry(Path::new("/a/b.PnG")));
        assert!(!is_png_entry(Path::new("/a/b.pngx")));
        assert!(!is_png_entry(Path::new("/a/b.txt")));
        assert!(!is_png_entry(Path::new("/a/png")));
        assert!(!is_png_entry(Path::new("/a/.png")));
    }
}
