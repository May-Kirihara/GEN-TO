//! gento-core [`ScanFs`] の実装 (MVP WP-2)。
//!
//! walkdirによる走査 (`scan::scan_images`)、PNGプローブ (`probe`)、
//! BLAKE3 (`hash`) を組み合わせる。ハッシュ優先度の制御 (Background)
//! は呼び出し側 (ワーカープール) の責務。

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use gento_core::library::{
    FileContentHash, HashFileError, ScanEntry, ScanFs, WalkError, WalkOutcome,
};

use crate::hash::blake3_file_hash;
use crate::probe::{probe_png_dimensions, ProbeResult};
use crate::scan::scan_images;

/// 既定の実装 (ステートレス)。
#[derive(Debug, Default, Clone, Copy)]
pub struct PlatformFs;

impl ScanFs for PlatformFs {
    fn walk(
        &self,
        root: &Path,
        scope: Option<&Path>,
        sink: &mut dyn FnMut(ScanEntry),
    ) -> WalkOutcome {
        let walk_root = match scope {
            Some(rel) => root.join(rel),
            None => root.to_path_buf(),
        };
        let mut outcome = WalkOutcome::default();
        let result = scan_images(&walk_root, &mut |image| {
            // rel_path は常にルート基準で解決する (scope基準ではない)。
            let rel_path = image
                .path
                .strip_prefix(root)
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|_| image.path.clone());
            sink(ScanEntry {
                rel_path,
                byte_size: image.size,
                mtime_ms: image.mtime_ms,
                os_file_id: image.os_file_id,
            });
        });
        outcome.entries_walked = result.entries;
        outcome.errors = result
            .errors
            .into_iter()
            .map(|e| {
                // ルート相対に解決 (解決不能な外部パスは空 = 全ブロック)。
                let rel_path = e
                    .path
                    .strip_prefix(root)
                    .map(|p| p.to_path_buf())
                    .unwrap_or_default();
                // パスが消えた (走査中削除・ルート不在) は実際の不在のため
                // missing化を妨げない。存在するが読めない (権限等) 場合のみ
                // 保守的にブロックする。
                let blocks_missing = e.path.exists();
                WalkError {
                    rel_path,
                    reason: e.reason,
                    blocks_missing,
                }
            })
            .collect();
        outcome
    }

    fn probe_dims(&self, path: &Path, expect: &FileContentHash) -> Option<(u32, u32)> {
        let matches_expect =
            |size: u64, mtime: u64| size == expect.byte_size && mtime == expect.mtime_ms;
        let before = std::fs::metadata(path).ok()?;
        if !matches_expect(
            before.len(),
            crate::scan::system_time_to_ms(before.modified()),
        ) {
            return None;
        }
        let dims = match probe_png_dimensions(path) {
            ProbeResult::Ok { width, height } => Some((width, height)),
            // PNG以外・読めない・壊れている: WP-2では寸法NULLで登録 (WP-3で形式拡張)
            _ => None,
        };
        let after = std::fs::metadata(path).ok()?;
        if !matches_expect(
            after.len(),
            crate::scan::system_time_to_ms(after.modified()),
        ) {
            return None;
        }
        dims
    }

    fn content_hash(
        &self,
        path: &Path,
        cancel: &AtomicBool,
    ) -> Result<FileContentHash, HashFileError> {
        // サービス側キャンセル (scan_stop) も即時反映するためRelaxedで十分
        // (停止の線形化は呼び出し側が管理)。
        let _ = Ordering::Relaxed;
        blake3_file_hash(path, cancel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "gento-platformfs-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn write(&self, rel: &str, bytes: &[u8]) -> PathBuf {
            if let Some(parent) = Path::new(rel).parent() {
                std::fs::create_dir_all(self.0.join(parent)).unwrap();
            }
            let p = self.0.join(rel);
            std::fs::write(&p, bytes).unwrap();
            p
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const BYTES: &[u8] = b"content";

    #[test]
    fn walk_resolves_rel_paths_against_root_even_in_scope() {
        let dir = TestDir::new("scope");
        dir.write("sub/a.png", BYTES);
        dir.write("sub/deep/b.png", BYTES);
        dir.write("other/c.png", BYTES);

        let fs = PlatformFs;
        let mut found = Vec::new();
        let outcome = fs.walk(&dir.0, Some(Path::new("sub")), &mut |e| {
            found.push(e.rel_path)
        });
        assert_eq!(outcome.entries_walked, 2);
        assert_eq!(found.len(), 2);
        assert!(found.contains(&PathBuf::from("sub/a.png")), "{found:?}");
        assert!(
            found.contains(&PathBuf::from("sub/deep/b.png")),
            "{found:?}"
        );
        assert!(!found.contains(&PathBuf::from("other/c.png")));
    }

    #[cfg(unix)]
    #[test]
    fn walk_collects_os_file_id() {
        let dir = TestDir::new("osfid");
        dir.write("a.png", BYTES);
        let fs = PlatformFs;
        let mut ids = Vec::new();
        fs.walk(&dir.0, None, &mut |e| ids.push(e.os_file_id));
        assert_eq!(ids.len(), 1);
        assert!(ids[0].is_some(), "Linuxでは dev+inode を取得");
    }

    #[test]
    fn walk_reports_missing_root_as_error() {
        let fs = PlatformFs;
        let outcome = fs.walk(Path::new("/nonexistent-gento-root"), None, &mut |_| {});
        assert_eq!(outcome.entries_walked, 0);
        assert_eq!(outcome.errors.len(), 1);
        // ルート自体が不在 = 実際の不在のためmissing化はブロックしない
        assert!(outcome.errors[0].rel_path.as_os_str().is_empty());
        assert!(!outcome.errors[0].blocks_missing);
        assert!(!outcome.errors[0].reason.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn walk_unreadable_subtree_blocks_missing_marking() {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("Uid:"))
                    .and_then(|l| l.split_whitespace().nth(1).map(|u| u == "0"))
            })
            .unwrap_or(false)
        {
            return; // rootは chmod 000 でも読めてしまうため検証にならない
        }
        let dir = TestDir::new("perm");
        dir.write("locked/inside.png", BYTES);
        let fs = PlatformFs;
        let mut found = Vec::new();
        std::fs::set_permissions(dir.0.join("locked"), std::fs::Permissions::from_mode(0o000))
            .unwrap();
        let outcome = fs.walk(&dir.0, None, &mut |e| found.push(e.rel_path.clone()));
        std::fs::set_permissions(dir.0.join("locked"), std::fs::Permissions::from_mode(0o755))
            .unwrap();

        assert!(found.is_empty(), "locked配下は列挙できない: {found:?}");
        let err = outcome
            .errors
            .iter()
            .find(|e| e.rel_path == *"locked")
            .expect("lockedの列挙エラー");
        assert!(err.blocks_missing, "存在するが読めない場合はブロック");
    }

    fn stat_expect(path: &std::path::PathBuf) -> FileContentHash {
        let md = std::fs::metadata(path).unwrap();
        FileContentHash {
            content_hash: String::new(),
            byte_size: md.len(),
            mtime_ms: crate::scan::system_time_to_ms(md.modified()),
        }
    }

    #[test]
    fn probe_dims_bound_to_expected_metadata() {
        let dir = TestDir::new("probe-meta");
        let png = dir.write("a.png", &crate::probe::minimal_png_bytes(640, 480));

        // 期待メタデータ一致 → 寸法を返す
        let expect = stat_expect(&png);
        assert_eq!(PlatformFs.probe_dims(&png, &expect), Some((640, 480)));

        // size不一致 → 検証失敗でNone (寸法は破棄)
        let mut stale_size = expect.clone();
        stale_size.byte_size += 1;
        assert_eq!(PlatformFs.probe_dims(&png, &stale_size), None);

        // mtime不一致 → None
        let mut stale_mtime = expect.clone();
        stale_mtime.mtime_ms += 1;
        assert_eq!(PlatformFs.probe_dims(&png, &stale_mtime), None);
    }

    #[test]
    fn probe_dims_only_png() {
        let dir = TestDir::new("probe");
        // PNGシグネチャのみの破損ファイル → None
        let broken = dir.write("broken.png", b"not a png");
        let expect = stat_expect(&broken);
        assert_eq!(PlatformFs.probe_dims(&broken, &expect), None);
        let txt = dir.write("note.txt", b"hello");
        let expect = stat_expect(&txt);
        assert_eq!(PlatformFs.probe_dims(&txt, &expect), None);
    }
}
