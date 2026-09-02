//! gento-core [`ScanFs`] の実装 (MVP WP-2 / FG-WP2-2)。
//!
//! walkdirによる走査 (`scan::scan_images`)、fd束ねのハッシュ+PNGプローブ
//! (`hash::hash_and_probe_file`) を組み合わせる。ハッシュ優先度の制御
//! (Background) は呼び出し側 (ワーカープール) の責務。

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use gento_core::library::{
    GenerationToken, HashFileError, HashedFile, ScanEntry, ScanFs, WalkError, WalkOutcome,
};

use crate::hash::{hash_and_probe_file, stat_token};
use crate::scan::scan_images;

/// 既定の実装 (ステートレス)。
#[derive(Debug, Default, Clone, Copy)]
pub struct PlatformFs;

/// 走査エラー対象パスの不在化可否 (在不在の積極的証明)。
///
/// `Path::exists()` は権限・メタデータエラーでもfalseを返すため使わない。
/// `symlink_metadata` の結果で分類する:
/// - `Ok(_)`: パスは存在する (読めなければblocking)
/// - `Err(NotFound)`: 不在が積極的に確定 (non-blocking)
/// - `Err(その他)`: 判定不能 (親ディレクトリが読めない等) → 保守的にblocking
fn error_path_blocks_missing(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(_) => true,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
    }
}

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
                mtime_ns: image.mtime_ns,
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
                // パスの不在を積極的に証明できた場合のみmissing化を許可。
                // 存在するが読めない (権限等)・判定不能は保守的にブロック。
                let blocks_missing = error_path_blocks_missing(&e.path);
                WalkError {
                    rel_path,
                    reason: e.reason,
                    blocks_missing,
                }
            })
            .collect();
        outcome
    }

    fn hash_and_probe(
        &self,
        path: &Path,
        expect: &GenerationToken,
        cancel: &AtomicBool,
    ) -> Result<HashedFile, HashFileError> {
        // サービス側キャンセル (scan_stop) も即時反映するためRelaxedで十分
        // (停止の線形化は呼び出し側が管理)。
        let _ = Ordering::Relaxed;
        hash_and_probe_file(path, expect, cancel)
    }

    fn hash_and_probe_and_metadata(
        &self,
        path: &Path,
        expect: &GenerationToken,
        cancel: &AtomicBool,
    ) -> Result<gento_core::library::HashedWithMetadata, HashFileError> {
        // WP-9: ハッシュ+プローブ+埋め込みメタデータ解析を同一fdで束ねる。
        crate::metadata::hash_probe_metadata_file(path, expect, cancel)
    }
}

/// プローブ単体の便宜ラッパ (既存の probe_dims 相当)。内部でfd束ねAPIを
/// 使用する — 現在のトークン観測が期待と一致する場合のみ寸法を返す。
pub fn probe_dims(path: &Path, expect: &GenerationToken) -> Option<(u32, u32)> {
    let current = stat_token(path)?;
    if current != *expect {
        return None;
    }
    match hash_and_probe_file(path, &current, &AtomicBool::new(false)) {
        Ok(hashed) => hashed.probe.map(|p| (p.width, p.height)),
        Err(_) => None,
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
    fn walk_collects_ns_precision_mtime() {
        // FG-WP2-2: 走査エントリはns精度のmtimeを観測する。
        let dir = TestDir::new("ns");
        let file = dir.write("a.png", BYTES);
        let fs = PlatformFs;
        let mut ns_values = Vec::new();
        fs.walk(&dir.0, None, &mut |e| ns_values.push(e.mtime_ns));
        assert_eq!(ns_values.len(), 1);
        assert_eq!(ns_values[0], stat_token(&file).unwrap().mtime_ns);
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

    // ===== 在不在の積極的証明 (3分岐) =====

    #[test]
    fn absence_classification_three_branches() {
        // (1) 存在するパス → blocking (読めなければ現状維持が正)
        let dir = TestDir::new("absence");
        let existing = dir.write("exists.png", BYTES);
        assert!(error_path_blocks_missing(&existing));

        // (2) NotFound → 不在確定 → non-blocking
        let gone = dir.0.join("definitely-gone.png");
        assert!(!error_path_blocks_missing(&gone));

        // (3) その他エラー (親ディレクトリが読めない等) → 判定不能 → blocking
        #[cfg(unix)]
        {
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
                return; // rootは権限エラーが起きないため検証にならない
            }
            let locked = dir.0.join("locked-dir");
            std::fs::create_dir_all(&locked).unwrap();
            std::fs::write(locked.join("child.png"), BYTES).unwrap();
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
            // 親が読めない子のstatはPermissionDenied (NotFoundではない) → blocking
            assert!(error_path_blocks_missing(&locked.join("child.png")));
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    // ===== fd束ねハッシュ+プローブ =====

    #[test]
    fn hash_and_probe_returns_bound_hash_and_dims() {
        let dir = TestDir::new("probe-meta");
        let png = dir.write("a.png", &crate::probe::minimal_png_bytes(640, 480));
        let fs = PlatformFs;

        // 期待トークン一致 → ハッシュ+寸法が1呼び出しで返る
        let expect = stat_token(&png).unwrap();
        let hashed = fs
            .hash_and_probe(&png, &expect, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(hashed.probe.map(|p| (p.width, p.height)), Some((640, 480)));
        assert_eq!(hashed.content.token(), expect);

        // size不一致 → 計算前にStale
        let mut stale = expect;
        stale.byte_size += 1;
        assert!(matches!(
            fs.hash_and_probe(&png, &stale, &AtomicBool::new(false)),
            Err(HashFileError::Stale(_))
        ));

        // ns不一致 (同一ms内差分) → Stale
        let mut stale_ns = expect;
        stale_ns.mtime_ns += 1;
        assert!(matches!(
            fs.hash_and_probe(&png, &stale_ns, &AtomicBool::new(false)),
            Err(HashFileError::Stale(_))
        ));
    }

    #[test]
    fn hash_and_probe_non_png_returns_no_dims() {
        let dir = TestDir::new("probe");
        let fs = PlatformFs;
        // PNGシグネチャのみの破損ファイル → 寸法None (ハッシュ自体は成功)
        let broken = dir.write("broken.png", b"not a png");
        let hashed = fs
            .hash_and_probe(
                &broken,
                &stat_token(&broken).unwrap(),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(hashed.probe, None);

        let txt = dir.write("note.txt", b"hello");
        let hashed = fs
            .hash_and_probe(&txt, &stat_token(&txt).unwrap(), &AtomicBool::new(false))
            .unwrap();
        assert_eq!(hashed.probe, None);
    }

    #[cfg(unix)]
    #[test]
    fn hash_and_probe_rejects_replaced_file_with_matching_ms() {
        // 同パス再利用 (新inode・同size・mtime完全一致) はos_file_id差でのみ
        // 検出できる — トークン不一致でStale。実fsでinode差し替えを決定的に
        // 再現する (旧観測トークンのまま新実体をハッシュしようとする場面)。
        let dir = TestDir::new("replace");
        let png = dir.write("a.png", &crate::probe::minimal_png_bytes(10, 10));
        let old_token = stat_token(&png).unwrap();
        let old_mtime = std::fs::metadata(&png).unwrap().modified().unwrap();

        // 同パスへ別実体を置換 (write→rename差し替えで新inode) し、mtimeを
        // 旧実体と完全一致させる (size・mtimeでは検出不能な敵対シナリオ)。
        let tmp = dir.0.join(".a.png.tmp");
        std::fs::write(&tmp, crate::probe::minimal_png_bytes(20, 30)).unwrap();
        std::fs::rename(&tmp, &png).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&png)
            .unwrap()
            .set_modified(old_mtime)
            .unwrap();

        let fs = PlatformFs;
        // 旧トークン (旧inode) での提出 → 新実体のfdトークンと不一致 → Stale
        assert!(
            matches!(
                fs.hash_and_probe(&png, &old_token, &AtomicBool::new(false)),
                Err(HashFileError::Stale(msg)) if msg.contains("before hashing")
            ),
            "置換後の実体を旧トークンでハッシュしようとしてもStale"
        );
        // 新実体の現在トークンなら成功し、新しい寸法が得られる
        let fresh = stat_token(&png).unwrap();
        assert_ne!(fresh.os_file_id, old_token.os_file_id, "inode差し替え前提");
        let hashed = fs
            .hash_and_probe(&png, &fresh, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(hashed.probe.map(|p| (p.width, p.height)), Some((20, 30)));
    }
}
