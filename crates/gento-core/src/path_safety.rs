//! 構造的パス安全プリミティブ (FG-WP13-2)。
//!
//! [`open_regular_beneath`] はルートディレクトリのfdからコンポーネント
//! 歩行で通常ファイルを開く。**どのコンポーネントもsymlinkを辿らない**
//! ため、`root/linked-dir/image.png` (linked-dirがルート外へのsymlink)
//! のような中間symlink脱出を構造的に遮断する (lexicな `..` 検査では
//! 防げない攻撃面 — WP-13レビューblocker)。
//!
//! - Linux: `openat2(2)` に `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS` を
//!   指定し、ルートdir fdからの相対パスを**単一の原子呼び出し**で解決
//!   する (カーネル ≥5.6)。ENOSYS/EINVAL (非対応カーネル・非対応構成)
//!   はfd歩行へフォールバックする。
//! - unix (フォールバック): ルートを `O_DIRECTORY|O_NOFOLLOW` で開いて
//!   fdをピン留めし、各中間コンポーネントを `openat(parent_fd, ...,
//!   O_DIRECTORY|O_NOFOLLOW)` で歩く (コンポーネントがsymlinkなら
//!   即エラー — `O_DIRECTORY` 有無によりELOOP/ENOTDIRが返る)、最終
//!   コンポーネントを `O_NOFOLLOW` で開く。fdは毎ステップ入れ替わる
//!   ため、歩行中にパスが差し替えられても既に開いたfdの外へは出られない。
//! - 非unix (Windows): 各prefixを `symlink_metadata` で字句検査する
//!   フォールバック。race-freeではない (構造的保証はunix実装のみ。
//!   Windows本対応はWP-17で再検討)。
//!
//! 経路は**一度だけ**解決される — 開いたfdとそのfstatが正本のため、
//! 「検査時statとopen後fstatの突き合わせ」のような二重解決のpin比較は
//! 不要である。最終コンポーネントのopenには `O_NONBLOCK` を含め、FIFO等
//! のopenブロッキングを起こさない (通常ファイル以外はfstatで拒否する
//! ため読み取りは発生せず、フラグの副作用も無い。確認後にフラグは落とす)。

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::path::Path;

/// `root` 直下を `components` (すべて単一のパス成分) で指定される
/// **通常ファイル**を開き、fd上のfstat metadataと共に返す。
///
/// 拒否: 経路中のどのコンポーネントのsymlinkも辿らない (中間・最終
/// 区別なし)。最終コンポーネントが通常ファイルでない (ディレクトリ・
/// FIFO・デバイス等)。コンポーネントの `.` / `..` / 区切り文字 (`/`、
/// 非unixでは `\\` も) / NULを含む値 (openatに複数成分として解釈させ
/// る悪用の防御)。
///
/// `root` 自体の最終成分がsymlinkの場合も拒否する (`O_NOFOLLOW` で
/// 開く — 呼び出し側は登録済みの実ディレクトリパスを渡すこと)。
pub fn open_regular_beneath(
    root: &Path,
    components: &[OsString],
) -> io::Result<(File, std::fs::Metadata)> {
    validate_components(components)?;
    #[cfg(target_os = "linux")]
    {
        if let Some(result) = openat2_regular_beneath(root, components) {
            return result;
        }
        // openat2非対応 (ENOSYS/EINVAL) → fd歩行へフォールバック
    }
    walk_open_regular_beneath(root, components)
}

/// `openat2(2)` による解決。`Some(result)` = syscallを実行した
/// (成否を含む)。`None` = 非対応のためfd歩行へフォールバック。
#[cfg(target_os = "linux")]
fn openat2_regular_beneath(
    root: &Path,
    components: &[OsString],
) -> Option<io::Result<(File, std::fs::Metadata)>> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::FromRawFd;

    let mut joined: Vec<u8> = Vec::new();
    for (i, comp) in components.iter().enumerate() {
        if i > 0 {
            joined.push(b'/');
        }
        joined.extend_from_slice(comp.as_bytes());
    }
    let path_c = match std::ffi::CString::new(joined) {
        Ok(c) => c,
        Err(e) => {
            return Some(Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("open_regular_beneath: {e}"),
            )))
        }
    };
    let root_fd = match open_root_fd(root) {
        Ok(fd) => fd,
        Err(e) => return Some(Err(e)),
    };
    // open_howは#[non_exhaustive]のためリテラル構築できない → ゼロ初期化
    // (全フィールドが整数で、ゼロは有効な既定値)。
    // SAFETY: 全整数フィールドのC構造体。ゼロ埋めは有効。
    let mut how: libc::open_how = unsafe { std::mem::zeroed() };
    how.flags = (libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK) as u64;
    how.mode = 0;
    how.resolve = libc::RESOLVE_BENEATH | libc::RESOLVE_NO_SYMLINKS;
    // SAFETY: syscall(2)にopenat2(2)の引数をそのまま渡す。sizeは構造体
    // 実サイズ (カーネルのsizeof検証と一致させる)。
    let rc = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            root_fd,
            path_c.as_ptr(),
            &how,
            std::mem::size_of::<libc::open_how>(),
        )
    };
    if rc == -1 {
        let err = io::Error::last_os_error();
        // SAFETY: root_fdの所有を手動で解放する (失敗経路)。
        unsafe { libc::close(root_fd) };
        match err.raw_os_error() {
            // カーネルがopenat2を知らない / 構成が受け付けない → 歩行へ
            Some(libc::ENOSYS) | Some(libc::EINVAL) => None,
            _ => Some(Err(err)),
        }
    } else {
        // SAFETY: 成功時のfdの所有権をFileへ移す (root_fdはclose済みで
        // ないため、ここで明示的に解放)。
        let file = unsafe { File::from_raw_fd(rc as libc::c_int) };
        // SAFETY: root_fdはfileとは別fdのため手動解放。
        unsafe { libc::close(root_fd) };
        Some(finish_regular(file))
    }
}

/// fd歩行による解決 (全unixで利用可能 — openat2非対応環境の正規経路)。
#[cfg(unix)]
fn walk_open_regular_beneath(
    root: &Path,
    components: &[OsString],
) -> io::Result<(File, std::fs::Metadata)> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::FromRawFd;

    let mut cur = open_root_fd(root)?;
    let last = components.len() - 1;
    for (i, comp) in components.iter().enumerate() {
        let c = std::ffi::CString::new(comp.as_bytes()).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("open_regular_beneath: {e}"),
            )
        })?;
        let flags = if i < last {
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC
        } else {
            // 最終: FIFO等のopenブロッキング回避 (通常ファイルでは無効)。
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK
        };
        // SAFETY: openat(2)はdirfd相対でpath文字列を読むだけ。
        let next = unsafe { libc::openat(cur, c.as_ptr(), flags) };
        if next < 0 {
            let err = io::Error::last_os_error();
            // SAFETY: 失敗経路のfd解放。
            unsafe { libc::close(cur) };
            return Err(err);
        }
        // SAFETY: 歩行fdの入れ替え (旧fd解放 → 新fdをparentにする)。
        unsafe { libc::close(cur) };
        cur = next;
    }
    // SAFETY: curの所有権をFileへ移す (以降の失敗はFileのDropが閉じる)。
    finish_regular(unsafe { File::from_raw_fd(cur) })
}

/// ルートをディレクトリfdとしてピン留めする (最終成分がsymlinkなら
/// ELOOP — 登録済み実ディレクトリのみ受け入れる)。
#[cfg(unix)]
fn open_root_fd(root: &Path) -> io::Result<libc::c_int> {
    use std::os::unix::ffi::OsStrExt;

    let root_c = std::ffi::CString::new(root.as_os_str().as_bytes()).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("open_regular_beneath: {e}"),
        )
    })?;
    // SAFETY: open(2)はpath文字列を読むだけ (O_CREAT無し = mode未使用)。
    let fd = unsafe {
        libc::open(
            root_c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(fd)
    }
}

/// 開いたfdのfstatで通常ファイルであることを確定し、`O_NONBLOCK` を
/// 除去して返す (metadataはfdピン留めされた実体のもの)。
#[cfg(unix)]
fn finish_regular(file: File) -> io::Result<(File, std::fs::Metadata)> {
    let md = file.metadata()?;
    if !md.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "open_regular_beneath: not a regular file",
        ));
    }
    // SAFETY: fcntl(2)のF_GETFL/F_SETFLはfd属性操作でポインタを扱わない。
    let fd = std::os::unix::io::AsRawFd::as_raw_fd(&file);
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags >= 0 {
        // SAFETY: 同上 (第4引数はint値)。
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) };
    }
    Ok((file, md))
}

#[cfg(unix)]
fn validate_components(components: &[OsString]) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;

    let invalid = |what: &str| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("open_regular_beneath: {what}"),
        )
    };
    if components.is_empty() {
        return Err(invalid("empty path"));
    }
    for comp in components {
        let bytes = comp.as_bytes();
        let bad = bytes.is_empty()
            || bytes == b"."
            || bytes == b".."
            || bytes.contains(&0)
            || bytes.contains(&b'/');
        if bad {
            return Err(invalid(&format!(
                "unacceptable component: {:?}",
                comp.to_string_lossy()
            )));
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_components(components: &[OsString]) -> io::Result<()> {
    let invalid = |what: &str| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("open_regular_beneath: {what}"),
        )
    };
    if components.is_empty() {
        return Err(invalid("empty path"));
    }
    for comp in components {
        let s = comp.to_string_lossy();
        if s.is_empty() || s == "." || s == ".." || s.contains('/') || s.contains('\\') {
            return Err(invalid(&format!("unacceptable component: {s:?}")));
        }
    }
    Ok(())
}

/// 字句的フォールバック (非unix)。各prefixの `symlink_metadata` で
/// symlink成分を弾いてから開く — TOCTOU窓が残る (構造的保証はunix
/// 実装のみ。WP-17のWindows対応で再検討)。
#[cfg(not(unix))]
fn walk_open_regular_beneath(
    root: &Path,
    components: &[OsString],
) -> io::Result<(File, std::fs::Metadata)> {
    let invalid = |what: &str| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("open_regular_beneath: {what}"),
        )
    };
    let mut cur = root.to_path_buf();
    let last = components.len() - 1;
    for (i, comp) in components.iter().enumerate() {
        cur.push(comp);
        let md = std::fs::symlink_metadata(&cur)?;
        if md.file_type().is_symlink() {
            return Err(invalid("symlink component"));
        }
        if i < last && !md.is_dir() {
            return Err(invalid("intermediate component is not a directory"));
        }
    }
    let file = std::fs::File::open(&cur)?;
    let md = file.metadata()?;
    if !md.is_file() {
        return Err(invalid("not a regular file"));
    }
    Ok((file, md))
}

#[cfg(test)]
mod tests {
    use super::*;

    // tempfile不使用規約 (std実装 — file_ops.rsのTestDirと同じ方式)
    struct TestDir(std::path::PathBuf);

    impl TestDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "gento-pathsafety-{tag}-{}-{}",
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
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn comps(parts: &[&str]) -> Vec<OsString> {
        parts.iter().map(|p| p.to_string().into()).collect()
    }

    #[test]
    fn opens_nested_regular_file_with_pinned_metadata() {
        let tmp = TestDir::new("nested");
        std::fs::create_dir_all(tmp.path().join("a/b")).unwrap();
        std::fs::write(tmp.path().join("a/b/c.png"), vec![7u8; 32]).unwrap();
        let (mut file, md) =
            open_regular_beneath(tmp.path(), &comps(&["a", "b", "c.png"])).unwrap();
        assert!(md.is_file());
        assert_eq!(md.len(), 32);
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut file, &mut buf).unwrap();
        assert_eq!(buf, vec![7u8; 32]);
        // O_NONBLOCKが残っていない (読み取りに影響しないが衛生上)
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
            assert_eq!(flags & libc::O_NONBLOCK, 0, "O_NONBLOCKは除去済み");
        }
    }

    #[test]
    #[cfg(unix)]
    fn intermediate_symlink_directory_is_not_traversed() {
        // ルート外の実体を指す中間symlinkを辿らない (blockerの本体)
        let tmp = TestDir::new("symdir");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("image.png"), vec![1u8; 64]).unwrap();
        std::fs::create_dir_all(tmp.path().join("root")).unwrap();
        std::os::unix::fs::symlink(&outside, tmp.path().join("root/linked-dir")).unwrap();

        let err = open_regular_beneath(
            &tmp.path().join("root"),
            &comps(&["linked-dir", "image.png"]),
        )
        .unwrap_err();
        // symlink成分のerrnoはELOOP (O_NOFOLLOW単体) またはENOTDIR
        // (O_DIRECTORY併用・openat2) — いずれも拒否が契約
        let code = err.raw_os_error();
        assert!(
            code == Some(libc::ELOOP) || code == Some(libc::ENOTDIR),
            "{err}"
        );
        // 直接のfd歩行でも同一 (フォールバック経路の担保)
        let err_walk = walk_open_regular_beneath(
            &tmp.path().join("root"),
            &comps(&["linked-dir", "image.png"]),
        )
        .unwrap_err();
        let code_walk = err_walk.raw_os_error();
        assert!(
            code_walk == Some(libc::ELOOP) || code_walk == Some(libc::ENOTDIR),
            "{err_walk}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn final_symlink_is_rejected() {
        let tmp = TestDir::new("symlast");
        std::fs::create_dir_all(tmp.path().join("root")).unwrap();
        std::fs::write(tmp.path().join("target.png"), vec![1u8; 8]).unwrap();
        std::os::unix::fs::symlink("../target.png", tmp.path().join("root/link.png")).unwrap();
        let err =
            open_regular_beneath(&tmp.path().join("root"), &comps(&["link.png"])).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::ELOOP), "{err}");
    }

    #[test]
    #[cfg(unix)]
    fn symlinked_root_is_rejected() {
        // ルート自体の最終成分がsymlinkならピン留めしない
        let tmp = TestDir::new("symroot");
        let real = tmp.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("a.png"), vec![1u8; 8]).unwrap();
        std::os::unix::fs::symlink(&real, tmp.path().join("rootlink")).unwrap();
        let err =
            open_regular_beneath(&tmp.path().join("rootlink"), &comps(&["a.png"])).unwrap_err();
        let code = err.raw_os_error();
        assert!(
            code == Some(libc::ELOOP) || code == Some(libc::ENOTDIR),
            "{err}"
        );
    }

    #[test]
    fn non_directory_intermediate_and_final_are_rejected() {
        let tmp = TestDir::new("notdir");
        std::fs::create_dir_all(tmp.path().join("root")).unwrap();
        std::fs::write(tmp.path().join("root/file.txt"), b"x").unwrap();
        // 中間がファイル
        let err = open_regular_beneath(&tmp.path().join("root"), &comps(&["file.txt", "deeper"]))
            .unwrap_err();
        #[cfg(unix)]
        assert_eq!(err.raw_os_error(), Some(libc::ENOTDIR), "{err}");
        // 最終がディレクトリ (= 通常ファイルではない)
        std::fs::create_dir_all(tmp.path().join("root/sub")).unwrap();
        let err = open_regular_beneath(&tmp.path().join("root"), &comps(&["sub"])).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{err}");
    }

    #[test]
    fn hostile_components_are_rejected_before_open() {
        let tmp = TestDir::new("hostile");
        std::fs::create_dir_all(tmp.path().join("root/a/b")).unwrap();
        std::fs::write(tmp.path().join("root/a/b/c.png"), b"x").unwrap();
        let root = tmp.path().join("root");
        for hostile in [
            vec![],
            comps(&[".."]),
            comps(&["."]),
            comps(&["a", "..", "b"]),
            comps(&["a/b", "c.png"]), // 区切り文字を含む成分
            comps(&["a", "", "b"]),   // 空成分
        ] {
            let err = open_regular_beneath(&root, &hostile)
                .err()
                .unwrap_or_else(|| panic!("{hostile:?} should be rejected"));
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{err}");
        }
        // 参照: 正常系は開ける
        assert!(open_regular_beneath(&root, &comps(&["a", "b", "c.png"])).is_ok());
    }

    #[test]
    #[cfg(unix)]
    fn missing_components_report_notfound() {
        let tmp = TestDir::new("missing");
        std::fs::create_dir_all(tmp.path().join("root")).unwrap();
        let err =
            open_regular_beneath(&tmp.path().join("root"), &comps(&["gone.png"])).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound, "{err}");
    }
}
