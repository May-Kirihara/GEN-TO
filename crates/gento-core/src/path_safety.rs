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
//!   フォールバック + [`is_lexically_beneath`] によるcanonicalize包含
//!   検証 (WP-17-1。race-freeではない — 構造的保証はunix実装のみ。
//!   制限は `docs/dev/platform-notes.md` 参照)。
//!
//! 経路は**一度だけ**解決される — 開いたfdとそのfstatが正本のため、
//! 「検査時statとopen後fstatの突き合わせ」のような二重解決のpin比較は
//! 不要である。最終コンポーネントのopenには `O_NONBLOCK` を含め、FIFO等
//! のopenブロッキングを起こさない (通常ファイル以外はfstatで拒否する
//! ため読み取りは発生せず、フラグの副作用も無い。確認後にフラグは落とす)。
//!
//! # 出力側包含 (FG-WP16-3 B1)
//!
//! [`OutputDir`] は**書き出し先**の包含を担保するハンドルである
//! (読み取り側の `open_regular_beneath` と対になる):
//! - 出力root ([`OutputDir::open_root`]) はユーザー指定パスを
//!   `O_DIRECTORY|O_NOFOLLOW` で開いてfd固定する (**最終成分がsymlink
//!   なら拒否** — UIで分かりやすくエラー報行するため)。
//! - サブディレクトリ ([`OutputDir::open_or_create_subdir`]) は
//!   `openat(.., O_DIRECTORY|O_NOFOLLOW)` の成分歩行で開き、ENOENTなら
//!   `mkdirat` して再openする (**作成とopenを競合安全に**: mkdirat成功→
//!   openat、EEXIST→openat。それがsymlinkならELOOPで拒否)。既存のどの
//!   成分がsymlinkでも辿らないため、`DEST/sub -> /outside` のような
//!   中間symlinkによる外部脱出を構造的に遮断する。
//! - ファイル公開 (tmp書き→`renameat2(RENAME_NOREPLACE)` のdirfd相対) も
//!   同fd基準で行うため、歩行後にパスが差し替えられてもfdの外へ出られない。

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::path::Path;

#[cfg(unix)]
use std::os::unix::io::FromRawFd;

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
/// 実装のみ)。
///
/// Windows ([`cfg(windows)`]) は追加で `std::fs::canonicalize` による
/// 包含検証を重ねる: 両辺の正規化実パスが [`is_lexically_beneath`]
/// (ドライブ文字・UNC・`\\?\`前置・大小同一視・区切り`\`/`/`両対応の
/// 純関数) を満たさなければ拒否する。symlink解決結果が包含を破る
/// (中間成分の差し替え等) 場合の追加防御 — canonicalizeとopenは別々の
/// 経路解決のため依然TOCTOU窓があり、best-effortである
/// (`docs/dev/platform-notes.md` の既知制限参照)。
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
    #[cfg(windows)]
    {
        let root_canon = std::fs::canonicalize(root)?;
        let final_canon = std::fs::canonicalize(&cur)?;
        if !is_lexically_beneath(
            &final_canon.to_string_lossy(),
            &root_canon.to_string_lossy(),
        ) {
            return Err(invalid("resolved path is not beneath the root"));
        }
    }
    let file = std::fs::File::open(&cur)?;
    let md = file.metadata()?;
    if !md.is_file() {
        return Err(invalid("not a regular file"));
    }
    Ok((file, md))
}

// ===== Windows字句的包含判定 (WP-17-1) =====
//
// 以下は**純関数**として全platformでコンパイルされる (cfg外)。
// Windows実機でテストを実行できないため、Linux上のユニットテストで
// 字句判定ロジックを実証する (`\\?\C:\` 形式を含む)。

/// Windowsパスの種別 (包含判定は種別一致が前提)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowsPathKind {
    /// `C:\…` (ドライブ文字は小文字正規化済み)。
    Drive(char),
    /// `\\server\share\…` (server/shareは成分列の先頭2要素)。
    Unc,
    /// 相対パス (包含の錨になれないため常に不一致扱い)。
    Relative,
}

/// Windowsパスを (種別, 小文字正規化成分列) へ分解する。
///
/// 正規化: `\\?\` / `\\?\UNC\` 前置の除去、区切り `/`・`\` の統一、
/// 大文字小文字の同一視 (NT名前空間はcase-insensitive)、`.` 成分の
/// 除去、`..` の字句的解決 (ルートより上はclamp — Windowsの実解決に
/// 合わせたbest-effort)。空成分 (重複区切り・末尾区切り) は無視。
fn parse_windows_path(input: &str) -> (WindowsPathKind, Vec<String>) {
    let lower = input.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return (WindowsPathKind::Relative, Vec::new());
    }
    let (kind, body) = if let Some(rest) = lower.strip_prefix(r"\\?\unc\") {
        // \\?\UNC\server\share → \\server\share (UNCへ戻す)
        (WindowsPathKind::Unc, format!(r"\\{rest}"))
    } else if let Some(rest) = lower.strip_prefix(r"\\?\") {
        parse_windows_body(rest)
    } else {
        parse_windows_body(&lower)
    };
    let mut comps: Vec<String> = body
        .split(['/', '\\'])
        .filter(|c| !c.is_empty() && *c != ".")
        .map(str::to_string)
        .collect();
    // `..` の字句的解決 (ルートより上はclamp)
    let mut resolved: Vec<String> = Vec::with_capacity(comps.len());
    for c in comps.drain(..) {
        if c == ".." {
            resolved.pop();
        } else {
            resolved.push(c);
        }
    }
    (kind, resolved)
}

/// `\\?\` を除去した本体の先頭から種別を判定する。
/// `C:` / `C:\…` / `C:/…` はドライブ (裸の`C:`もドライブルート扱い —
/// best-effort)、`\\…` はUNC、それ以外は相対。
fn parse_windows_body(body: &str) -> (WindowsPathKind, String) {
    let bytes = body.as_bytes();
    if bytes.len() >= 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes.len() == 2 || bytes[2] == b'\\' || bytes[2] == b'/')
    {
        // ドライブ文字は成分列から除き種別へ格納する
        return (
            WindowsPathKind::Drive(bytes[0] as char),
            body[2..].to_string(),
        );
    }
    if let Some(rest) = body.strip_prefix(r"\\") {
        return (WindowsPathKind::Unc, format!(r"\\{rest}"));
    }
    (WindowsPathKind::Relative, body.to_string())
}

/// `path` が `root` 配下 (root自身を含む) に字句的に収まるか
/// (Windowsセマンティクス: 大小同一視・区切り`\`/`/`両対応・
/// `\\?\` verbatim前置の透過的除去・`..`の字句的解決)。
///
/// - ドライブ文字 (`C:` vs `D:`) とUNC server/share は完全一致が必須。
/// - 相対パスは錨になれないため、いずれかの辺が相対ならfalse。
/// - 等値 (path == root) は包含扱い (包含検証の用途上、境界を含む)。
///
/// 制限 (best-effort・`docs/dev/platform-notes.md` 参照): 8.3短縮名・
/// 末尾ドット/空白のWin32正規化・NTFSストリーム構文は解釈しない。
/// この関数は**字句判定のみ**であり、いかなるTOCTOU耐性も提供しない。
pub fn is_lexically_beneath(path: &str, root: &str) -> bool {
    let (path_kind, path_comps) = parse_windows_path(path);
    let (root_kind, root_comps) = parse_windows_path(root);
    if !matches!(path_kind, WindowsPathKind::Drive(_) | WindowsPathKind::Unc)
        || !matches!(root_kind, WindowsPathKind::Drive(_) | WindowsPathKind::Unc)
    {
        return false;
    }
    if path_kind != root_kind {
        return false;
    }
    if path_comps.len() < root_comps.len() {
        return false;
    }
    root_comps
        .iter()
        .zip(path_comps.iter())
        .all(|(r, p)| r == p)
}

// ===== 出力側包含 (FG-WP16-3 B1) =====

/// [`OutputDir::rename_noreplace_at`] の失敗分類。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameAtNoReplaceError {
    /// 宛先が既に存在する (上書きしない)。
    DestinationExists,
    /// その他のIO失敗。
    Io(String),
}

impl std::fmt::Display for RenameAtNoReplaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenameAtNoReplaceError::DestinationExists => {
                write!(f, "destination already exists (no-clobber)")
            }
            RenameAtNoReplaceError::Io(msg) => write!(f, "rename_at io error: {msg}"),
        }
    }
}

impl std::error::Error for RenameAtNoReplaceError {}

/// 書き出し先用の包含担保ディレクトリハンドル。
///
/// - unix: ピン留めされたdirectory fd (`O_DIRECTORY|O_NOFOLLOW` で開いた
///   実ディレクトリ)。配下への操作はすべてfd相対 (`openat`/`mkdirat`/
///   `unlinkat`/`renameat2`) のため、パス名の差し替え競合から構造的に独立。
/// - 非unix: 字句検査 (`symlink_metadata`) 付きのパジス操作フォールバック
///   (TOCTOU窓が残る — 構造的保証はunix実装のみ。WP-17のWindows対応で再検討)。
///
/// `display` はエラー報告用の表示パス (lossy)。
#[derive(Debug)]
pub struct OutputDir {
    #[cfg(unix)]
    fd: File,
    #[cfg(not(unix))]
    path: std::path::PathBuf,
    display: String,
}

impl OutputDir {
    /// 出力root (ユーザー指定dir) を開く。不在なら作成してから開く
    /// (親構成はユーザー指定パスの一部 — `create_dir_all`)。
    /// **最終成分がsymlinkなら拒否** (ELOOP / 非unixは字句拒否)。
    pub fn open_root(dest: &Path) -> io::Result<Self> {
        #[cfg(unix)]
        {
            if let Err(e) = std::fs::symlink_metadata(dest) {
                if e.kind() == io::ErrorKind::NotFound {
                    std::fs::create_dir_all(dest)?;
                } else {
                    return Err(e);
                }
            }
            let file = open_root_fd(dest)?;
            Ok(Self {
                // SAFETY: open_root_fd成功時のfd所有権をFileへ移す。
                fd: unsafe { File::from_raw_fd(file) },
                display: dest.display().to_string(),
            })
        }
        #[cfg(not(unix))]
        {
            match std::fs::symlink_metadata(dest) {
                Ok(md) if md.file_type().is_symlink() => Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "OutputDir::open_root: destination is a symlink",
                )),
                Ok(md) if !md.is_dir() => Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "OutputDir::open_root: destination is not a directory",
                )),
                Ok(_) => Ok(Self {
                    path: dest.to_path_buf(),
                    display: dest.display().to_string(),
                }),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    std::fs::create_dir_all(dest)?;
                    Ok(Self {
                        path: dest.to_path_buf(),
                        display: dest.display().to_string(),
                    })
                }
                Err(e) => Err(e),
            }
        }
    }

    /// 既存のdirのみを開く (作成しない — dry-run/preview用)。
    pub fn open_existing_root(dest: &Path) -> io::Result<Self> {
        #[cfg(unix)]
        {
            let file = open_root_fd(dest)?;
            Ok(Self {
                // SAFETY: 同上。
                fd: unsafe { File::from_raw_fd(file) },
                display: dest.display().to_string(),
            })
        }
        #[cfg(not(unix))]
        {
            let md = std::fs::symlink_metadata(dest)?;
            if md.file_type().is_symlink() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "OutputDir::open_existing_root: destination is a symlink",
                ));
            }
            if !md.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "OutputDir::open_existing_root: destination is not a directory",
                ));
            }
            Ok(Self {
                path: dest.to_path_buf(),
                display: dest.display().to_string(),
            })
        }
    }

    /// `components` (単一成分列。空可) をopen-or-createで辿ったsubdirの
    /// ハンドルを返す。**既存のどの成分がsymlinkでも拒否**する。
    /// 作成は `mkdirat`→`openat(O_NOFOLLOW)` の競合安全な手順。
    pub fn open_or_create_subdir(&self, components: &[OsString]) -> io::Result<Self> {
        let display = join_display(&self.display, components);
        #[cfg(unix)]
        {
            validate_components_opt(components)?;
            // SAFETY: fd操作はraw fd整数の読み取りのみ。
            let mut cur = unsafe { libc::dup(std::os::unix::io::AsRawFd::as_raw_fd(&self.fd)) };
            if cur < 0 {
                return Err(io::Error::last_os_error());
            }
            for comp in components {
                match openat_dir(cur, comp) {
                    Ok(next) => {
                        // SAFETY: 歩行fdの入れ替え (旧fd解放)。
                        unsafe { libc::close(cur) };
                        cur = next;
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        // mkdirat→openat (EEXISTはopenatで再判定 —
                        // その際にsymlinkならO_NOFOLLOWがELOOPで拒否)
                        if let Some(rc) = mkdirat_raw(cur, comp) {
                            if rc < 0 && errno() != libc::EEXIST {
                                let err = io::Error::last_os_error();
                                // SAFETY: 失敗経路のfd解放。
                                unsafe { libc::close(cur) };
                                return Err(err);
                            }
                        }
                        match openat_dir(cur, comp) {
                            Ok(next) => {
                                // SAFETY: 同上。
                                unsafe { libc::close(cur) };
                                cur = next;
                            }
                            Err(e) => {
                                // SAFETY: 同上。
                                unsafe { libc::close(cur) };
                                return Err(e);
                            }
                        }
                    }
                    Err(e) => {
                        // SAFETY: 同上。
                        unsafe { libc::close(cur) };
                        return Err(e);
                    }
                }
            }
            Ok(Self {
                // SAFETY: 歩行済みfdの所有権をFileへ。
                fd: unsafe { File::from_raw_fd(cur) },
                display,
            })
        }
        #[cfg(not(unix))]
        {
            let mut cur = self.path.clone();
            for comp in components {
                cur.push(comp);
                match std::fs::symlink_metadata(&cur) {
                    Ok(md) if md.file_type().is_symlink() => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!("OutputDir: symlink component {:?}", comp.to_string_lossy()),
                        ));
                    }
                    Ok(md) if !md.is_dir() => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "OutputDir: component is not a directory",
                        ));
                    }
                    Ok(_) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        std::fs::create_dir(&cur)?;
                    }
                    Err(e) => return Err(e),
                }
            }
            Ok(Self { path: cur, display })
        }
    }

    /// 既存のsubdirのみを開く (作成しない — dry-run/preview用)。
    pub fn open_existing_subdir(&self, components: &[OsString]) -> io::Result<Self> {
        let display = join_display(&self.display, components);
        #[cfg(unix)]
        {
            validate_components_opt(components)?;
            // SAFETY: fd読み取りのみ。
            let mut cur = unsafe { libc::dup(std::os::unix::io::AsRawFd::as_raw_fd(&self.fd)) };
            if cur < 0 {
                return Err(io::Error::last_os_error());
            }
            for comp in components {
                match openat_dir(cur, comp) {
                    Ok(next) => {
                        // SAFETY: 歩行fdの入れ替え。
                        unsafe { libc::close(cur) };
                        cur = next;
                    }
                    Err(e) => {
                        // SAFETY: 失敗経路の解放。
                        unsafe { libc::close(cur) };
                        return Err(e);
                    }
                }
            }
            Ok(Self {
                // SAFETY: 歩行済みfdの所有権をFileへ。
                fd: unsafe { File::from_raw_fd(cur) },
                display,
            })
        }
        #[cfg(not(unix))]
        {
            let mut cur = self.path.clone();
            for comp in components {
                cur.push(comp);
                let md = std::fs::symlink_metadata(&cur)?;
                if md.file_type().is_symlink() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("OutputDir: symlink component {:?}", comp.to_string_lossy()),
                    ));
                }
                if !md.is_dir() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "OutputDir: component is not a directory",
                    ));
                }
            }
            Ok(Self { path: cur, display })
        }
    }

    /// 配下の既存**通常ファイル**を `O_NOFOLLOW` で開く (既存分類用)。
    /// 最終成分がsymlinkならELOOP (呼び出し側でConflict扱い等に分類)。
    pub fn open_regular_at(&self, name: &OsStr) -> io::Result<(File, std::fs::Metadata)> {
        validate_single_component(name)?;
        #[cfg(unix)]
        {
            // SAFETY: fd読み取りのみ。
            let parent = std::os::unix::io::AsRawFd::as_raw_fd(&self.fd);
            // FIFO等のopenブロッキング回避 (通常ファイルでは無効。fstatで
            // 拒否するため読み取りは発生しない。確認後にフラグは落とす)。
            let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
            // SAFETY: openat(2)はdirfd相対でpath文字列を読むだけ。
            let rc = unsafe { libc::openat(parent, os_cstring(name)?.as_ptr(), flags) };
            if rc < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: 成功時のfd所有権をFileへ。
            finish_regular(unsafe { File::from_raw_fd(rc) })
        }
        #[cfg(not(unix))]
        {
            let target = self.path.join(name);
            let md = std::fs::symlink_metadata(&target)?;
            if md.file_type().is_symlink() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "OutputDir: final component is a symlink",
                ));
            }
            let file = std::fs::File::open(&target)?;
            let md = file.metadata()?;
            if !md.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "OutputDir: not a regular file",
                ));
            }
            Ok((file, md))
        }
    }

    /// 配下へ排他作成 (`O_CREAT|O_EXCL` — tmp書き出し用。既存名や
    /// symlink名とは決して衝突しない)。
    pub fn create_exclusive_at(&self, name: &OsStr) -> io::Result<File> {
        validate_single_component(name)?;
        #[cfg(unix)]
        {
            // SAFETY: fd読み取りのみ。
            let parent = std::os::unix::io::AsRawFd::as_raw_fd(&self.fd);
            let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC;
            // SAFETY: openat(2)。O_CREAT付きのためmode (0o644) を渡す。
            // modeはvariadic引数のためc_uintで渡す (Linuxのmode_t=u32と
            // 一致。darwinのmode_t=u16は可変長引数で昇格必須 — E0617)。
            let rc = unsafe {
                libc::openat(
                    parent,
                    os_cstring(name)?.as_ptr(),
                    flags,
                    0o644 as libc::c_uint,
                )
            };
            if rc < 0 {
                Err(io::Error::last_os_error())
            } else {
                // SAFETY: 成功時のfd所有権をFileへ。
                Ok(unsafe { File::from_raw_fd(rc) })
            }
        }
        #[cfg(not(unix))]
        {
            let target = self.path.join(name);
            if let Ok(md) = std::fs::symlink_metadata(&target) {
                let _ = md;
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "OutputDir: name already exists",
                ));
            }
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
        }
    }

    /// 配下内のno-clobber rename (`renameat2(RENAME_NOREPLACE)` のdirfd相対。
    /// 非対応カーネル/FSは `linkat`+`unlinkat` フォールバック — 同じ原子性)。
    pub fn rename_noreplace_at(
        &self,
        from: &OsStr,
        to: &OsStr,
    ) -> Result<(), RenameAtNoReplaceError> {
        validate_single_component(from).map_err(err_io)?;
        validate_single_component(to).map_err(err_io)?;
        #[cfg(unix)]
        {
            // SAFETY: fd読み取りのみ。
            let dirfd = std::os::unix::io::AsRawFd::as_raw_fd(&self.fd);
            #[cfg(target_os = "linux")]
            if let Some(result) = renameat2_noreplace_at(dirfd, from, to) {
                return result;
            }
            // renameat2非対応 (または非linux) → linkat+unlinkatへ
            link_unlink_at(dirfd, from, to)
        }
        #[cfg(not(unix))]
        {
            let src = self.path.join(from);
            let dst = self.path.join(to);
            if let Err(e) = std::fs::hard_link(&src, &dst) {
                return Err(match e.kind() {
                    io::ErrorKind::AlreadyExists => RenameAtNoReplaceError::DestinationExists,
                    _ => RenameAtNoReplaceError::Io(format!(
                        "{} -> {}: {e}",
                        src.display(),
                        dst.display()
                    )),
                });
            }
            std::fs::remove_file(&src).map_err(|e| {
                RenameAtNoReplaceError::Io(format!("{} (link済み): {e}", src.display()))
            })
        }
    }

    /// 配下のファイルを削除 (tmp破棄用)。
    pub fn remove_file_at(&self, name: &OsStr) -> io::Result<()> {
        validate_single_component(name)?;
        #[cfg(unix)]
        {
            // SAFETY: fd読み取りのみ。
            let dirfd = std::os::unix::io::AsRawFd::as_raw_fd(&self.fd);
            // SAFETY: unlinkat(2)はdirfd相対でpath文字列を読むだけ。
            let rc = unsafe { libc::unlinkat(dirfd, os_cstring(name)?.as_ptr(), 0) };
            if rc < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
        #[cfg(not(unix))]
        {
            std::fs::remove_file(self.path.join(name))
        }
    }

    /// エラー報告用の表示パス (lossy)。
    pub fn display(&self) -> &str {
        &self.display
    }
}

fn err_io(e: io::Error) -> RenameAtNoReplaceError {
    RenameAtNoReplaceError::Io(e.to_string())
}

/// 表示パスの結合 (エラー報告用)。
fn join_display(base: &str, components: &[OsString]) -> String {
    let mut out = base.to_string();
    for comp in components {
        out.push('/');
        out.push_str(&comp.to_string_lossy());
    }
    out
}

/// 成分列の検査 (空列を許可 — root自体を指す)。
#[cfg(unix)]
fn validate_components_opt(components: &[OsString]) -> io::Result<()> {
    if components.is_empty() {
        return Ok(());
    }
    validate_components(components)
}

/// 単一成分の検査 (区切り文字・`.`・`..`・NUL拒否)。
#[cfg(unix)]
fn validate_single_component(name: &OsStr) -> io::Result<()> {
    let comps = [name.to_os_string()];
    validate_components(&comps)
}

#[cfg(not(unix))]
fn validate_single_component(name: &OsStr) -> io::Result<()> {
    let comps = [name.to_os_string()];
    validate_components(&comps)
}

/// unix: OsStrをCStringへ (NUL含みはエラー)。
#[cfg(unix)]
fn os_cstring(name: &OsStr) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(name.as_bytes())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, format!("OutputDir: {e}")))
}

/// unix: dirfd相対でディレクトリを開く (`O_DIRECTORY|O_NOFOLLOW`)。
/// 成功時は新fd (呼び出し側で旧fdを閉じる)。
#[cfg(unix)]
fn openat_dir(parent: libc::c_int, name: &OsStr) -> io::Result<libc::c_int> {
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY: openat(2)はdirfd相対でpath文字列を読むだけ。
    let rc = unsafe { libc::openat(parent, os_cstring(name)?.as_ptr(), flags) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(rc)
    }
}

/// unix: mkdiratの生戻り値。`None` = 引数変換失敗。
#[cfg(unix)]
fn mkdirat_raw(parent: libc::c_int, name: &OsStr) -> Option<libc::c_int> {
    match os_cstring(name) {
        Ok(c) => {
            // SAFETY: mkdirat(2)。mode 0o755 (umask適用はカーネル側)。
            Some(unsafe { libc::mkdirat(parent, c.as_ptr(), 0o755 as libc::mode_t) })
        }
        Err(_) => None,
    }
}

/// 直近のerrno。
#[cfg(unix)]
fn errno() -> libc::c_int {
    io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// `renameat2(RENAME_NOREPLACE)` のdirfd相対呼び出し。
/// `Some(result)` = syscallを実行した。`None` = 非対応 (フォールバックへ)。
#[cfg(all(unix, target_os = "linux"))]
fn renameat2_noreplace_at(
    dirfd: libc::c_int,
    from: &OsStr,
    to: &OsStr,
) -> Option<Result<(), RenameAtNoReplaceError>> {
    // libc版による差異を避けるためflag値を直に定義
    // (linux/renameat2.h: RENAME_NOREPLACE = 1 << 0)。
    const RENAME_NOREPLACE: libc::c_uint = 1;
    let c_from = match os_cstring(from) {
        Ok(c) => c,
        Err(e) => return Some(Err(RenameAtNoReplaceError::Io(e.to_string()))),
    };
    let c_to = match os_cstring(to) {
        Ok(c) => c,
        Err(e) => return Some(Err(RenameAtNoReplaceError::Io(e.to_string()))),
    };
    // SAFETY: syscall(2)にrenameat2(2)の引数をそのまま渡す。
    let rc = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            dirfd,
            c_from.as_ptr(),
            dirfd,
            c_to.as_ptr(),
            RENAME_NOREPLACE,
        )
    };
    if rc == 0 {
        return Some(Ok(()));
    }
    let err = io::Error::last_os_error();
    match err.raw_os_error() {
        Some(libc::EEXIST) => Some(Err(RenameAtNoReplaceError::DestinationExists)),
        Some(libc::ENOSYS) | Some(libc::EINVAL) | Some(libc::EOPNOTSUPP) => None,
        _ => Some(Err(RenameAtNoReplaceError::Io(format!(
            "{} -> {}: {err}",
            from.to_string_lossy(),
            to.to_string_lossy()
        )))),
    }
}

/// `linkat`+`unlinkat` によるフォールバック (linkは宛先存在時にEEXISTで
/// 失敗する原子操作のためno-clobber契約を満たす)。
#[cfg(unix)]
fn link_unlink_at(
    dirfd: libc::c_int,
    from: &OsStr,
    to: &OsStr,
) -> Result<(), RenameAtNoReplaceError> {
    let c_from = os_cstring(from).map_err(err_io)?;
    let c_to = os_cstring(to).map_err(err_io)?;
    // SAFETY: linkat(2)はdirfd相対でpath文字列を読むだけ。
    let rc = unsafe { libc::linkat(dirfd, c_from.as_ptr(), dirfd, c_to.as_ptr(), 0) };
    if rc < 0 {
        let err = io::Error::last_os_error();
        return Err(match err.raw_os_error() {
            Some(libc::EEXIST) => RenameAtNoReplaceError::DestinationExists,
            _ => RenameAtNoReplaceError::Io(format!(
                "{} -> {}: {err}",
                from.to_string_lossy(),
                to.to_string_lossy()
            )),
        });
    }
    // SAFETY: unlinkat(2)はdirfd相対でpath文字列を読むだけ。
    let rc = unsafe { libc::unlinkat(dirfd, c_from.as_ptr(), 0) };
    if rc < 0 {
        let err = io::Error::last_os_error();
        return Err(RenameAtNoReplaceError::Io(format!(
            "{} (link済みのため両名に存在。手動で片側を削除してください): {err}",
            from.to_string_lossy()
        )));
    }
    Ok(())
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
        // ルート外の実体を指す中間symlinkを辿らない (blockerの本体)。
        // macOSではopenat2が存在しないため open_regular_beneath は常に
        // fd歩行 (walk_open_regular_beneath) へ落ちる — このテストは
        // 公開APIと歩行関数の双方を同じ攻撃面で検証することで、
        // macOS経路 (=歩行のみ) の保護も接続確認する (WP-17-1)。
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

    // ===== WP-17-1: Windows字句的包含判定 (純関数 — Linux上で実証) =====

    #[test]
    fn lexical_beneath_matches_case_insensitive_and_both_separators() {
        assert!(is_lexically_beneath(r"C:\Users\name\img.png", r"C:\Users"));
        assert!(is_lexically_beneath("C:/users/name/img.png", r"C:\Users"));
        assert!(is_lexically_beneath(r"c:\USERS\name", r"C:\users\"));
        // 大文字小文字混在も同一視
        assert!(is_lexically_beneath(
            r"C:\Users\Name\IMG.png",
            r"c:\users\NAME"
        ));
    }

    #[test]
    fn lexical_beneath_strips_verbatim_and_unc_prefixes() {
        // canonicalize出力 (\\?\前置) と通常表現の相互運用
        assert!(is_lexically_beneath(
            r"\\?\C:\Users\name\img.png",
            r"C:\Users"
        ));
        assert!(is_lexically_beneath(
            r"C:\Users\name\img.png",
            r"\\?\C:\Users"
        ));
        // UNCのverbatim形 (\\?\UNC\server\share ↔ \\server\share)
        assert!(is_lexically_beneath(
            r"\\?\UNC\server\share\p\img.png",
            r"\\server\share"
        ));
        assert!(is_lexically_beneath(
            r"\\server\share\p\img.png",
            r"\\?\UNC\server\share"
        ));
        // ドライブルート直下
        assert!(is_lexically_beneath(r"\\?\C:\x.png", r"C:\"));
    }

    #[test]
    fn lexical_beneath_rejects_cross_drive_unc_and_prefix_boundary() {
        // ドライブ不一致
        assert!(!is_lexically_beneath(r"D:\Users\name", r"C:\Users"));
        // UNC server/share不一致
        assert!(!is_lexically_beneath(r"\\other\share\p", r"\\server\share"));
        assert!(!is_lexically_beneath(
            r"\\?\UNC\other\share\p",
            r"\\server\share"
        ));
        // 文字列接頭で切らない (成分境界)
        assert!(!is_lexically_beneath(r"C:\Users Warfare\x", r"C:\Users"));
        // .. は字句解決される (C:\evil への脱出は拒否)
        assert!(!is_lexically_beneath(
            r"C:\Users\name\..\..\evil",
            r"C:\Users"
        ));
        assert!(is_lexically_beneath(r"C:\Users\name\..\other", r"C:\Users"));
    }

    #[test]
    fn lexical_beneath_equal_is_contained_and_relative_never_matches() {
        // 等値は包含扱い (境界を含む)
        assert!(is_lexically_beneath(r"C:\Users", r"C:\Users"));
        // ドライブのみroot
        assert!(is_lexically_beneath(r"C:\x", r"C:"));
        // 相対パスは錨になれない
        assert!(!is_lexically_beneath(r"Users\name", r"C:\Users"));
        assert!(!is_lexically_beneath(r"C:\Users\name", r"Users"));
        assert!(!is_lexically_beneath("", r"C:\"));
    }

    // ===== FG-WP16-3 B1: 出力側包含 =====

    #[test]
    #[cfg(unix)]
    fn output_dir_open_root_creates_missing_and_rejects_symlink() {
        let tmp = TestDir::new("outroot");
        // 不在でも作成して開ける
        let root = OutputDir::open_root(&tmp.path().join("new/deeper")).unwrap();
        assert!(tmp.path().join("new/deeper").is_dir());
        assert_eq!(
            root.display(),
            tmp.path().join("new/deeper").display().to_string()
        );
        // root最終成分がsymlinkなら拒否 (ELOOP or ENOTDIR — O_DIRECTORY併用時)
        let real = tmp.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(&real, tmp.path().join("link")).unwrap();
        let err = OutputDir::open_root(&tmp.path().join("link")).unwrap_err();
        let code = err.raw_os_error();
        assert!(
            code == Some(libc::ELOOP) || code == Some(libc::ENOTDIR),
            "{err}"
        );
        let err = OutputDir::open_existing_root(&tmp.path().join("link")).unwrap_err();
        let code = err.raw_os_error();
        assert!(
            code == Some(libc::ELOOP) || code == Some(libc::ENOTDIR),
            "{err}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn open_or_create_dir_beneath_creates_and_walks_nofollow() {
        let tmp = TestDir::new("outwalk");
        let root = OutputDir::open_root(&tmp.path().join("dest")).unwrap();
        // 作成しつつ歩く
        root.open_or_create_subdir(&comps(&["a", "b"])).unwrap();
        assert!(tmp.path().join("dest/a/b").is_dir());
        // 既存経路の再歩行も成功 (冪等)
        root.open_or_create_subdir(&comps(&["a", "b"])).unwrap();
        // 空列 = root自身
        let same = root.open_or_create_subdir(&[]).unwrap();
        // ハンドルからfile出版各種が動作する
        let mut out = same.create_exclusive_at(&OsString::from("f.txt")).unwrap();
        std::io::Write::write_all(&mut out, b"data").unwrap();
        out.sync_all().unwrap();
        drop(out);
        same.rename_noreplace_at(&OsString::from("f.txt"), &OsString::from("g.txt"))
            .unwrap();
        assert_eq!(
            std::fs::read(tmp.path().join("dest/g.txt")).unwrap(),
            b"data"
        );
        let (file, md) = same.open_regular_at(&OsString::from("g.txt")).unwrap();
        assert!(md.is_file());
        drop(file);
        same.remove_file_at(&OsString::from("g.txt")).unwrap();
        assert!(!tmp.path().join("dest/g.txt").exists());
    }

    /// 既存成分がsymlinkなら歩行拒否 — `DEST/sub -> /outside` 脱出の構造的遮断。
    /// dir差し替え後も同様 (O_NOFOLLOWの防御がmkdirat競合でも効くことの
    /// 成分レベル検証)。
    #[test]
    #[cfg(unix)]
    fn open_or_create_dir_beneath_rejects_symlink_component() {
        let tmp = TestDir::new("outsym");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let root = OutputDir::open_root(&tmp.path().join("dest")).unwrap();
        std::os::unix::fs::symlink(&outside, tmp.path().join("dest/sub")).unwrap();

        let err = root
            .open_or_create_subdir(&comps(&["sub", "dir"]))
            .unwrap_err();
        let code = err.raw_os_error();
        assert!(
            code == Some(libc::ELOOP) || code == Some(libc::ENOTDIR),
            "{err}"
        );
        // 外部には何も作られない
        assert!(std::fs::read_dir(&outside).unwrap().next().is_none());

        // 実dirを作って歩いた後、それをsymlinkへ差し替えると再歩行は拒否
        let root2 = OutputDir::open_root(&tmp.path().join("dest2")).unwrap();
        root2.open_or_create_subdir(&comps(&["a"])).unwrap();
        std::fs::remove_dir(tmp.path().join("dest2/a")).unwrap();
        std::os::unix::fs::symlink(&outside, tmp.path().join("dest2/a")).unwrap();
        let err = root2
            .open_or_create_subdir(&comps(&["a", "b"]))
            .unwrap_err();
        let code = err.raw_os_error();
        assert!(
            code == Some(libc::ELOOP) || code == Some(libc::ENOTDIR),
            "{err}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn output_dir_open_regular_and_create_exclusive_edge() {
        let tmp = TestDir::new("outexcl");
        let root = OutputDir::open_root(&tmp.path().join("dest")).unwrap();
        // open_regular_at: 不在はNotFound
        let err = root
            .open_regular_at(&OsString::from("gone.png"))
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound, "{err}");
        // 最終成分symlinkはELOOP拒否 (openを辿らない)
        let target = tmp.path().join("target.png");
        std::fs::write(&target, b"x").unwrap();
        std::os::unix::fs::symlink(&target, tmp.path().join("dest/link.png")).unwrap();
        let err = root
            .open_regular_at(&OsString::from("link.png"))
            .unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::ELOOP), "{err}");
        // create_exclusive_at: 同名二度目はAlreadyExists (symlink名も保護)
        let f = root.create_exclusive_at(&OsString::from("a.tmp")).unwrap();
        drop(f);
        let err = root
            .create_exclusive_at(&OsString::from("a.tmp"))
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists, "{err}");
        let err = root
            .create_exclusive_at(&OsString::from("link.png"))
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists, "{err}");
        // ディレクトリ名も作成不可
        std::fs::create_dir(tmp.path().join("dest/sub")).unwrap();
        let err = root
            .create_exclusive_at(&OsString::from("sub"))
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists, "{err}");
        // 不正成分 (区切り文字等) は事前拒否
        for bad in ["a/b", "..", "."] {
            let err = root.create_exclusive_at(&OsString::from(bad)).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{bad}: {err}");
        }
    }

    #[test]
    #[cfg(unix)]
    fn rename_noreplace_at_moves_and_refuses_existing() {
        let tmp = TestDir::new("outren");
        let root = OutputDir::open_root(&tmp.path().join("dest")).unwrap();
        std::fs::write(tmp.path().join("dest/a"), b"1").unwrap();
        root.rename_noreplace_at(&OsString::from("a"), &OsString::from("b"))
            .unwrap();
        assert_eq!(std::fs::read(tmp.path().join("dest/b")).unwrap(), b"1");
        // 宛先既存はDestinationExists (既存ファイル無傷)
        std::fs::write(tmp.path().join("dest/c"), b"existing").unwrap();
        std::fs::write(tmp.path().join("dest/d"), b"new").unwrap();
        let err = root
            .rename_noreplace_at(&OsString::from("d"), &OsString::from("c"))
            .unwrap_err();
        assert_eq!(err, RenameAtNoReplaceError::DestinationExists, "{err}");
        assert_eq!(
            std::fs::read(tmp.path().join("dest/c")).unwrap(),
            b"existing"
        );
        // フォールバック (linkat+unlinkat) の直接検証 — 契約同一
        let err = link_unlink_at(
            std::os::unix::io::AsRawFd::as_raw_fd(&root_open_fd(&tmp.path().join("dest"))),
            &OsString::from("d"),
            &OsString::from("c"),
        )
        .unwrap_err();
        assert!(matches!(err, RenameAtNoReplaceError::DestinationExists));
    }

    /// テスト専用: destをO_NOFOLLOWで開いたFile (raw fd取得用)。
    #[cfg(unix)]
    fn root_open_fd(path: &Path) -> File {
        let fd = open_root_fd(path).unwrap();
        // SAFETY: fd所有権をFileへ。
        unsafe { File::from_raw_fd(fd) }
    }
}
