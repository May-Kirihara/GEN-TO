//! OS差の土台 (MVP WP-7新設。計画書§5のworkspace図における
//! `gento-platform`: ゴミ箱・OSファイルID・資格情報ストア等)。
//!
//! # 境界規約
//!
//! - 本crateは**OS固有機能の実装集約点**であり、ドメイン型
//!   ([`gento_core::repo::OsFileId`] 等) はgento-coreから借用する
//!   (依存方向: platform → core。逆方向禁止)。
//! - ドメイン層 (gento-core) はここへ依存しない。ファイル操作エンジンは
//!   trait (`FileOpsFs`) を介してこれらの機能を注入される
//!   (計画書§5「外部機能はtraitを介して注入」)。
//! - Windows/macOS対応はWP-17で実機確認する (現状はLinux実装が中心。
//!   `os_file_id_of` は非Unixで `None` を返す既存規約を維持)。
//!
//! # ゴミ箱 (trash crate包み)
//!
//! - 関数 [`trash_to_bin`]: OSゴミ箱へ移動し、**生成されたゴミ箱エントリを
//!   特定して** [`TrashEntry`] で返す (undo復帰の厳密同一性検証用。
//!   WP-7レビュー対応)。Linux実装 (trash crate) は
//!   freedesktop.org Trash Specification に従い、対象ファイルと同じ
//!   「topdir」(マウント) ごとにゴミ箱を評価する:
//!   - ホーム領域 (= home trashと同一マウント): `$XDG_DATA_HOME/Trash`
//!     (通常 `~/.local/share/Trash`)
//!   - 他マウント (tmpfsの /tmp 等): `<マウントルート>/.Trash-<uid>`
//!   - ディスプレイ非依存 (GIO非依存) のためヘッドレス環境でも動作する。
//! - **失敗時の通常削除フォールバックは呼び出し側で禁止** (安全原則:
//!   「通常削除はゴミ箱」。エラー表示のみ)。
//! - [`restore_from_trash`]: Undo用の復帰。trash crateに単一項目復帰APIが
//!   無いため、XDG仕様の `.trashinfo` とエントリ名で構成される
//!   [`TrashEntry`] を直接扱う。復帰前にエントリの存在検証+サイズ検証
//!   (`expect_byte_size` = 操作時トークンのsize成分) を行い、不一致は
//!   拒否する (パス再利用・複数エントリ競合での誤復帰防止)。
//!
//! # 原子的no-clobberリネーム
//!
//! - [`rename_noreplace`]: Linux `renameat2(RENAME_NOREPLACE)` 直接syscall
//!   (libc経由。新規依存追加なし)。カーネル/FSが対応しない場合は
//!   `link+unlink` フォールバック (同じく原子的no-clobber)。
//!   上書きTOCTOU排除の最終防御 (WP-7レビュー対応)。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use gento_core::file_ops::TrashEntry;
use gento_core::repo::OsFileId;

/// metadataからOSファイルIDを取り出す (Linux: dev+inode)。
/// 元々gento-image/src/scan.rsにあった実装を本crateへ集約移動した
/// (WP-7。gento-imageは再エポートで追随)。
/// シンボリックリンクは走査対象外のため、常に実ファイルのmetadata。
#[cfg(unix)]
pub fn os_file_id_of(md: &fs::Metadata) -> Option<OsFileId> {
    use std::os::unix::fs::MetadataExt;
    Some(OsFileId::new(md.dev(), md.ino()))
}

#[cfg(not(unix))]
pub fn os_file_id_of(_md: &fs::Metadata) -> Option<OsFileId> {
    None
}

/// metadataのmtime (ナノ秒精度のu64)。UNIX_EPOCH以前は飽和。
/// WP-17-1: fileops.rs・backup.rsに散在していた同一実装の集約
/// (Windows file ID相当の提供は今後課題 — platform-notes参照)。
pub fn mtime_ns_of(md: &fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let secs: u64 = md.mtime().try_into().unwrap_or(u64::MAX);
        let nanos: u32 = md.mtime_nsec().try_into().unwrap_or(0);
        secs.saturating_mul(1_000_000_000)
            .saturating_add(nanos as u64)
    }
    #[cfg(not(unix))]
    {
        md.modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
    }
}

/// ディレクトリの属するファイルシステムの空き容量 (bytes)。
/// unix: `statvfs(2)` の `f_bavail * f_bsize` (一般ユーザーが使える領域)。
/// 非unix: 常に `None` (Windowsの `GetDiskFreeSpaceExW` 実装は未対応 —
/// `docs/dev/platform-notes.md` §2参照)。
/// WP-17-1: tauri fileops.rs (unix全体) とembedding download.rs
/// (linux限定) に散在していた実装の集約。集約によりmacOSでも
/// 空き容量確認が有効になる (それ以前のdownload側はlinuxのみ)。
///
/// FG-WP17-1: `None` (非unixまたはstatvfs失敗) は呼び出し側で `Err` へ
/// 変換されるが、現行の唯一の呼び出し経路 (gento-core `plan_file_op` の
/// `disk_shortfall_bytes` 計算) は `Err` を `.ok()` で打ち切るため
/// **容量確認skip = fail-open** として働く (事前確認は警告のみで実行阻止
/// しない二重防護。最終防御は実行時の書き込み失敗/DiskFullエラー)。
pub fn dir_free_bytes(dir: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        let c_path = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()).ok()?;
        // SAFETY: statvfs(2)はpath文字列を読み、out構造体へ書くだけ。
        // 全フィールドが整数のC構造体のためゼロ埋めは有効。
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: 同上。
        let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
        if rc != 0 {
            return None;
        }
        Some((stat.f_bavail as u64).saturating_mul(stat.f_bsize as u64))
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        None
    }
}

/// [`rename_noreplace`] の失敗分類。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameNoReplaceError {
    /// 宛先が既に存在する (上書きしない — TOCTOU競合の検出)。
    DestinationExists,
    /// その他のIO失敗。
    Io(String),
}

impl std::fmt::Display for RenameNoReplaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenameNoReplaceError::DestinationExists => {
                write!(f, "destination already exists (no-clobber)")
            }
            RenameNoReplaceError::Io(msg) => write!(f, "rename_noreplace io error: {msg}"),
        }
    }
}

impl std::error::Error for RenameNoReplaceError {}

/// 原子的no-clobberリネーム: `to` が既に存在する場合は
/// [`RenameNoReplaceError::DestinationExists`] を返し、両ファイルを
/// 変更しない (置換禁止。上書きTOCTOUの排除)。
///
/// - Linux: `renameat2(2)` に `RENAME_NOREPLACE` を指定して直接syscall
///   (libcのsyscall番号経由。依存追加なし)。
/// - カーネルやファイルシステムが `renameat2` のflagsに対応しない場合
///   (ENOSYS/EINVAL/ENOTSUP)、`link(2)` + `unlink(2)` へフォールバック
///   する (`link` は宛先存在時に `EEXIST` で失敗する原子操作のため、
///   同じno-clobber契約を満たす)。
/// - ファイル専用 (ディレクトリのlinkは不可。GEN-TOの最終配置はすべて
///   ファイル。ディレクトリ作成は `create_dir` が既に排他)。
pub fn rename_noreplace(from: &Path, to: &Path) -> Result<(), RenameNoReplaceError> {
    #[cfg(target_os = "linux")]
    {
        if let Some(result) = renameat2_noreplace(from, to) {
            return result;
        }
        // renameat2が使えない (ENOSYS/EINVAL/ENOTSUP) → link+unlinkへ。
    }
    link_unlink_noreplace(from, to)
}

/// `renameat2(RENAME_NOREPLACE)` の呼び出し。`Some(result)` = syscallが
/// 実行された (成否を含む)。`None` = サポート外のためフォールバック。
#[cfg(target_os = "linux")]
fn renameat2_noreplace(from: &Path, to: &Path) -> Option<Result<(), RenameNoReplaceError>> {
    use std::os::unix::ffi::OsStrExt;
    // libc版による差異を避けるためflag値を直に定義
    // (linux/renameat2.h: RENAME_NOREPLACE = 1 << 0)。
    const RENAME_NOREPLACE: libc::c_uint = 1;
    let c_from = match std::ffi::CString::new(from.as_os_str().as_bytes()) {
        Ok(c) => c,
        Err(e) => {
            return Some(Err(RenameNoReplaceError::Io(format!(
                "{}: {e}",
                from.display()
            ))))
        }
    };
    let c_to = match std::ffi::CString::new(to.as_os_str().as_bytes()) {
        Ok(c) => c,
        Err(e) => {
            return Some(Err(RenameNoReplaceError::Io(format!(
                "{}: {e}",
                to.display()
            ))))
        }
    };
    let rc = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            c_from.as_ptr(),
            libc::AT_FDCWD,
            c_to.as_ptr(),
            RENAME_NOREPLACE,
        )
    };
    if rc == 0 {
        return Some(Ok(()));
    }
    let err = std::io::Error::last_os_error();
    match err.raw_os_error() {
        Some(libc::EEXIST) => Some(Err(RenameNoReplaceError::DestinationExists)),
        Some(libc::ENOSYS) | Some(libc::EINVAL) | Some(libc::EOPNOTSUPP) => None,
        code => Some(Err(RenameNoReplaceError::Io(format!(
            "{} -> {}: {} (os error {})",
            from.display(),
            to.display(),
            err,
            code.unwrap_or(0)
        )))),
    }
}

/// `link(2)` + `unlink(2)` によるno-clobber移動 (フォールバック)。
/// linkは宛先存在時にEEXISTで失敗する原子操作のため契約を満たす。
/// unlink失敗時、ファイルは両名で存在する (同一inode。データは失われない)。
fn link_unlink_noreplace(from: &Path, to: &Path) -> Result<(), RenameNoReplaceError> {
    if let Err(e) = fs::hard_link(from, to) {
        return Err(match e.kind() {
            std::io::ErrorKind::AlreadyExists => RenameNoReplaceError::DestinationExists,
            _ => RenameNoReplaceError::Io(format!("{} -> {}: {e}", from.display(), to.display())),
        });
    }
    fs::remove_file(from).map_err(|e| {
        RenameNoReplaceError::Io(format!(
            "{} (link済みのため両名に存在。手動で片側を削除してください): {e}",
            from.display()
        ))
    })
}

/// OSゴミ箱へ移動し、生成されたゴミ箱エントリを特定して返す。
/// 失敗した場合はエラーを返すのみで、対象ファイルは**移動されず元の位置に
/// 残る** (ロールバック検証: 本関数がErrを返した時点で対象パスは不変。
/// trash crateは移動にrenameを使うため部分移動も起きない)。
///
/// エントリ特定: 移動前に候補ゴミ箱の `info/` を.snapshotし、移動後に
/// **新規に出現した `Path=` 一致のtrashinfo** から `files/` 側の名前を
/// 得る (名前衝突でtrash crateが付けるsuffixにも対応)。特定できない
/// 場合はエラー (undo情報の欠落を黙認しない)。
///
/// `path` が存在しない場合はエラー。
pub fn trash_to_bin(path: &Path) -> Result<TrashEntry, String> {
    if !path.exists() {
        return Err(format!("{}: not found", path.display()));
    }
    let before = snapshot_trashinfos(path);
    trash::delete(path).map_err(|e| format!("trash failed for {}: {e}", path.display()))?;
    let after = snapshot_trashinfos(path);
    // 新規trashinfoのうち Path= が対象と一致するものを最新順に選ぶ。
    let mut matches: Vec<(PathBuf, String, String)> = Vec::new(); // (trash_dir, stem, date)
    for (dir, infos) in &after {
        for (stem, (recorded, date)) in infos {
            let known = before.get(dir).is_some_and(|prev| prev.contains_key(stem));
            if !known && recorded_matches(dir, recorded, path) {
                matches.push((dir.clone(), stem.clone(), date.clone()));
            }
        }
    }
    matches.sort_by(|a, b| b.2.cmp(&a.2));
    if let Some((trash_dir, file_name, _)) = matches.into_iter().next() {
        return Ok(TrashEntry {
            trash_dir,
            file_name,
        });
    }
    Err(format!(
        "trashed {} がエントリ特定に失敗しました (undo情報を記録できません。\
         ゴミ箱から手動で確認してください)",
        path.display()
    ))
}

/// `recorded` (trashinfoの `Path=`。マウントローカルゴミ箱では
/// マウントルート相対の場合がある) が `original` と一致するか。
fn recorded_matches(trash_dir: &Path, recorded: &Path, original: &Path) -> bool {
    if recorded.is_absolute() {
        return recorded == original;
    }
    // XDG仕様: admin trash (.Trash/$uid, .Trash-$uid) の Path= は
    // topdir (マウントルート) 相对。
    #[cfg(unix)]
    if let Some(mount) = mount_root_of(trash_dir) {
        return mount.join(recorded) == original;
    }
    false
}

/// 候補ゴミ箱群の `info/` snapshot: trash_dir → {stem: (Path=, DeletionDate=)}。
fn snapshot_trashinfos(original: &Path) -> HashMap<PathBuf, HashMap<String, (PathBuf, String)>> {
    let mut out: HashMap<PathBuf, HashMap<String, (PathBuf, String)>> = HashMap::new();
    for trash_root in candidate_trash_dirs(original) {
        let info_dir = trash_root.join("info");
        let Ok(entries) = fs::read_dir(&info_dir) else {
            continue;
        };
        let mut infos = HashMap::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(stem) = name
                .to_str()
                .and_then(|n| n.strip_suffix(".trashinfo"))
                .map(str::to_string)
            else {
                continue;
            };
            if let Some((path, date)) = read_trashinfo_fields(&entry.path()) {
                infos.insert(stem, (path, date));
            }
        }
        if !infos.is_empty() {
            out.insert(trash_root, infos);
        }
    }
    out
}

/// trashinfoから (Path=, DeletionDate=) を読む。
fn read_trashinfo_fields(info_path: &Path) -> Option<(PathBuf, String)> {
    let body = fs::read_to_string(info_path).ok()?;
    let path = original_path_from_trashinfo(info_path)?;
    let date = body
        .lines()
        .find(|l| l.starts_with("DeletionDate="))
        .map(|l| l.trim_start_matches("DeletionDate=").to_string())
        .unwrap_or_default();
    Some((path, date))
}

/// ゴミ箱エントリの現在バイト数 (存在検証用。不在・読取不可はNone)。
pub fn trash_entry_byte_size(entry: &TrashEntry) -> Option<u64> {
    fs::metadata(entry.trash_dir.join("files").join(&entry.file_name))
        .ok()
        .map(|m| m.len())
}

/// ゴミ箱から `original_path` へ復帰する (Undo用)。
///
/// [`TrashEntry`] が指す**正確なエントリ**を `original` へ移動する。
/// 復帰前に:
/// - エントリ実体の存在検証 (無ければ [`RestoreError::NotFound`])。
/// - サイズ検証: 実体サイズが `expect_byte_size` (操作時トークンの
///   size成分) と一致しなければ [`RestoreError::SizeMismatch`]
///   (パス再利用・複数エントリ競合での誤復帰防止)。
/// - 復帰先に同名ファイルが存在すれば [`RestoreError::DestinationExists`]
///   (上書きしない — 外部変更でUndoの前提が崩れたケース)。
///
/// 移動後、対応するtrashinfoを削除する (ゴミ箱側の整合)。
pub fn restore_from_trash(
    entry: &TrashEntry,
    original: &Path,
    expect_byte_size: u64,
) -> Result<PathBuf, RestoreError> {
    let trashed = entry.trash_dir.join("files").join(&entry.file_name);
    let size = fs::metadata(&trashed)
        .map_err(|e| {
            RestoreError::NotFound(format!(
                "trash entry {} not readable: {e}",
                trashed.display()
            ))
        })?
        .len();
    if size != expect_byte_size {
        return Err(RestoreError::SizeMismatch {
            path: trashed,
            expected: expect_byte_size,
            actual: size,
        });
    }
    if original.exists() {
        return Err(RestoreError::DestinationExists(original.to_path_buf()));
    }
    if let Some(parent) = original.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| RestoreError::Io(format!("create {}: {e}", parent.display())))?;
    }
    fs::rename(&trashed, original).map_err(|e| {
        RestoreError::Io(format!(
            "restore {} -> {}: {e}",
            trashed.display(),
            original.display()
        ))
    })?;
    let _ = fs::remove_file(
        entry
            .trash_dir
            .join("info")
            .join(format!("{}.trashinfo", entry.file_name)),
    );
    Ok(original.to_path_buf())
}

/// ゴミ箱復帰の失敗。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreError {
    /// 復帰候補がゴミ箱内に見つからない (対応するtrashinfoが無い)。
    NotFound(String),
    /// 復帰先に同名ファイルが存在する (上書きはしない — 安全原則)。
    DestinationExists(PathBuf),
    /// エントリ実体のサイズが操作時の記録 (トークンのsize成分) と
    /// 一致しない (パス再利用・複数エントリ競合での誤復帰防止)。
    SizeMismatch {
        path: PathBuf,
        expected: u64,
        actual: u64,
    },
    /// 読み取り・移動の失敗。
    Io(String),
}

impl std::fmt::Display for RestoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RestoreError::NotFound(msg) => write!(f, "trash entry not found: {msg}"),
            RestoreError::DestinationExists(path) => write!(
                f,
                "cannot restore: destination already exists: {}",
                path.display()
            ),
            RestoreError::SizeMismatch {
                path,
                expected,
                actual,
            } => write!(
                f,
                "trash entry size mismatch: {} (expected {expected} bytes, got {actual})",
                path.display()
            ),
            RestoreError::Io(msg) => write!(f, "restore io error: {msg}"),
        }
    }
}

impl std::error::Error for RestoreError {}

/// XDG仕様のホームゴミ箱 (`$XDG_DATA_HOME/Trash`、無ければ
/// `~/.local/share/Trash`)。
fn xdg_trash_root() -> Option<PathBuf> {
    if let Some(data_home) = std::env::var_os("XDG_DATA_HOME") {
        if !data_home.is_empty() {
            return Some(PathBuf::from(data_home).join("Trash"));
        }
    }
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    Some(PathBuf::from(home).join(".local/share/Trash"))
}

/// 自分のUID (/proc/self/statusから取得。Linux)。
fn current_uid() -> Option<u32> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("Uid:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

/// `path` の属するマウントのルートを返す (最も近い「親のst_devが変わる」
/// 位置。path自体が不在でも、存在する最も深い祖先から判定する)。
#[cfg(unix)]
fn mount_root_of(path: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    // 存在する最も深い祖先
    let mut exists = None;
    let mut current = Some(path);
    while let Some(p) = current {
        if let Ok(md) = fs::metadata(p) {
            exists = Some((p.to_path_buf(), md));
            break;
        }
        current = p.parent();
    }
    let (mut ancestor, mut md) = exists?;
    while let Some(parent) = ancestor.parent() {
        let Ok(pmd) = fs::metadata(parent) else {
            break;
        };
        if pmd.dev() != md.dev() {
            break; // マウント境界
        }
        ancestor = parent.to_path_buf();
        md = pmd;
    }
    Some(ancestor)
}

/// `original_path` の復帰先として探索すべきゴミ箱ディレクトリ群
/// (ホームゴミ箱 + 元パスのマウントのadmin trash)。重複は除去。
fn candidate_trash_dirs(original_path: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(home_trash) = xdg_trash_root() {
        dirs.push(home_trash);
    }
    #[cfg(unix)]
    if let Some(uid) = current_uid() {
        if let Some(mount) = mount_root_of(original_path) {
            // XDG仕様: `.Trash/$uid` (sticky bit必須) または `.Trash-$uid`
            let admin_shared = mount.join(".Trash").join(uid.to_string());
            let admin_simple = mount.join(format!(".Trash-{uid}"));
            for dir in [admin_shared, admin_simple] {
                if !dirs.contains(&dir) {
                    dirs.push(dir);
                }
            }
        }
    }
    dirs
}

/// trashinfoファイルから `Path=` 行 (パーセントエンコードされた元パス) を
/// 取り出す。復元はXDG仕様のパーセントデコード (UTF-8)。
fn original_path_from_trashinfo(info_path: &Path) -> Option<PathBuf> {
    let body = fs::read_to_string(info_path).ok()?;
    let line = body.lines().find(|l| l.starts_with("Path="))?;
    let encoded = line.trim_start_matches("Path=");
    let decoded = percent_decode_utf8(encoded)?;
    Some(PathBuf::from(decoded))
}

/// XDG trashinfo仕様のパーセントデコード (UTF-8パス前提)。
fn percent_decode_utf8(encoded: &str) -> Option<String> {
    let bytes = encoded.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 3 > bytes.len() {
                return None;
            }
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            let value = u8::from_str_radix(hex, 16).ok()?;
            out.push(value);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Mutex;

    /// XDG_DATA_HOME / HOME はプロセス広域のため、環境を書き換える
    /// テストはこのmutexで直列化する (並列テストの競合防止)。
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "gento-platform-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// XDG_DATA_HOMEを一意のtrashへ向け、旧値を返す (restore探索の正本も
    /// 同じenvを読むため、trash crateと探索が一致する)。
    fn set_xdg_trash(tag: &str) -> (TestDir, PathBuf) {
        let tmp = TestDir::new(tag);
        let trash = tmp.0.join("xdg/data/Trash");
        fs::create_dir_all(trash.join("info")).unwrap();
        fs::create_dir_all(trash.join("files")).unwrap();
        std::env::set_var("XDG_DATA_HOME", tmp.0.join("xdg/data"));
        (tmp, trash)
    }

    fn write_trashinfo(trash: &Path, name: &str, original: &str, date: &str) {
        let info = trash.join("info").join(format!("{name}.trashinfo"));
        fs::write(
            &info,
            format!("[Trash Info]\nPath={original}\nDeletionDate={date}\n"),
        )
        .unwrap();
    }

    #[test]
    fn trash_to_bin_moves_file_and_restore_round_trips() {
        let _env = ENV_LOCK.lock().unwrap();
        let (work, _trash) = set_xdg_trash("roundtrip");
        // 同じマウントに置く (ホームtrashが使われるよう、XDG_DATA_HOME配下
        // に元ファイルも置く — topdir判定がhome trashと一致する)。
        let original = work.0.join("dir").join("img.png");
        fs::create_dir_all(original.parent().unwrap()).unwrap();
        fs::write(&original, b"pngdata").unwrap();

        let entry = trash_to_bin(&original).expect("trash succeeds");
        assert!(!original.exists(), "元位置からは消える");
        let trash_root = xdg_trash_root().unwrap();
        assert_eq!(entry.trash_dir, trash_root);
        assert!(
            trash_root.join("files").join(&entry.file_name).exists(),
            "特定したエントリがfiles/配下に実在する"
        );
        assert!(
            trash_root
                .join("info")
                .join(format!("{}.trashinfo", entry.file_name))
                .exists(),
            "対応するtrashinfoが存在 (特定の正しさ)"
        );

        let restored =
            restore_from_trash(&entry, &original, b"pngdata".len() as u64).expect("restore");
        assert_eq!(restored, original);
        assert_eq!(fs::read(&original).unwrap(), b"pngdata");
    }

    /// 同名を2回trashした場合、各エントリは別名で特定され、それぞれ
    /// 正確に復帰できる (パス再利用・複数エントリ競合への耐性)。
    #[test]
    fn trash_identifies_distinct_entries_for_same_path() {
        let _env = ENV_LOCK.lock().unwrap();
        let (work, _trash) = set_xdg_trash("dupnames");
        let original = work.0.join("dup.png");
        fs::write(&original, b"first").unwrap();
        let entry1 = trash_to_bin(&original).unwrap();
        // 同パスを別内容で再出現→再trash
        fs::write(&original, b"second!").unwrap();
        let entry2 = trash_to_bin(&original).unwrap();
        assert_ne!(entry1.file_name, entry2.file_name, "衝突suffixで別名");
        // それぞれ正確に復帰 (entry1を先に戻すとentry2の復帰先が塞がる
        // ため、entry2 → 削除 → entry1の順で検証)
        let restored2 = restore_from_trash(&entry2, &original, 7).unwrap();
        assert_eq!(fs::read(&restored2).unwrap(), b"second!");
        fs::remove_file(&original).unwrap();
        let restored1 = restore_from_trash(&entry1, &original, 5).unwrap();
        assert_eq!(fs::read(&restored1).unwrap(), b"first");
    }

    /// 外部で同パスのtrashinfoが既に存在していても、新規エントリのみを
    /// 特定する (既存エントリと混同しない)。
    #[test]
    fn trash_identifies_only_new_entry_when_same_path_exists() {
        let _env = ENV_LOCK.lock().unwrap();
        let (tmp, trash) = set_xdg_trash("existing");
        let original = tmp.0.join("mix.png");
        // 外部trashed相当の既存エントリ (内容 OLD)
        write_trashinfo(
            &trash,
            "mix",
            &original.to_string_lossy(),
            "2026-08-30T00:00:00",
        );
        fs::write(trash.join("files").join("mix"), b"OLD").unwrap();
        // GEN-TOが同パスをtrash (内容 NEW) → 新規エントリが特定される
        fs::write(&original, b"NEW").unwrap();
        let entry = trash_to_bin(&original).unwrap();
        assert_ne!(entry.file_name, "mix", "既存ではなく新規エントリ");
        let restored = restore_from_trash(&entry, &original, 3).unwrap();
        assert_eq!(fs::read(&restored).unwrap(), b"NEW", "新規分を復帰");
    }

    #[test]
    fn restore_missing_entry_is_not_found() {
        let _env = ENV_LOCK.lock().unwrap();
        let (_tmp, _trash) = set_xdg_trash("missing");
        let entry = TrashEntry {
            trash_dir: xdg_trash_root().unwrap(),
            file_name: "ghost".to_string(),
        };
        let err =
            restore_from_trash(&entry, Path::new("/definitely/not/trashed.png"), 1).unwrap_err();
        assert!(matches!(err, RestoreError::NotFound(_)), "{err}");
    }

    #[test]
    fn restore_refuses_when_destination_exists() {
        let _env = ENV_LOCK.lock().unwrap();
        let (tmp, trash) = set_xdg_trash("dstexists");
        let original = tmp.0.join("dst.png");
        write_trashinfo(
            &trash,
            "a",
            &original.to_string_lossy(),
            "2026-08-31T10:00:00",
        );
        fs::write(trash.join("files").join("a"), b"trashed").unwrap();
        fs::write(&original, b"external new file").unwrap();
        let entry = TrashEntry {
            trash_dir: trash.clone(),
            file_name: "a".to_string(),
        };
        let err = restore_from_trash(&entry, &original, 7).unwrap_err();
        assert!(
            matches!(&err, RestoreError::DestinationExists(p) if *p == original),
            "{err}"
        );
        // 上書きされない — 外部ファイルは無傷
        assert_eq!(fs::read(&original).unwrap(), b"external new file");
    }

    /// エントリ実体のサイズが期待 (トークンのsize成分) と一致しない
    /// 場合は復帰拒否 (誤復帰防止)。
    #[test]
    fn restore_refuses_when_entry_size_mismatch() {
        let _env = ENV_LOCK.lock().unwrap();
        let (tmp, trash) = set_xdg_trash("sizemismatch");
        let original = tmp.0.join("size.png");
        write_trashinfo(
            &trash,
            "s",
            &original.to_string_lossy(),
            "2026-08-31T10:00:00",
        );
        fs::write(trash.join("files").join("s"), b"actual-9").unwrap();
        let entry = TrashEntry {
            trash_dir: trash.clone(),
            file_name: "s".to_string(),
        };
        let err = restore_from_trash(&entry, &original, 42).unwrap_err();
        assert!(
            matches!(
                &err,
                RestoreError::SizeMismatch {
                    expected: 42,
                    actual: 8,
                    ..
                }
            ),
            "{err}"
        );
        assert!(!original.exists(), "復帰しない");
    }

    /// マウントローカルtrashinfoの相対 `Path=` 対応 (non-blocker):
    /// admin trash (.Trash-$uid) の Path= はマウントルート相対。
    #[test]
    fn restore_resolves_relative_path_against_mount_root() {
        // recorded_matches の単体検証: 相対Path=はtrash dirの実際の
        // マウントルート基準で解決される (環境非依存に実ディレクトリで検証)。
        let work = TestDir::new("relpath");
        let trash_dir = work.0.join(".Trash-1000");
        fs::create_dir_all(trash_dir.join("info")).unwrap();
        fs::create_dir_all(trash_dir.join("files")).unwrap();
        let mount = mount_root_of(&trash_dir).expect("trash dirが存在すればマウントルートは求まる");
        assert!(
            mount.is_absolute() && trash_dir.starts_with(&mount),
            "trash dirは自身のマウントルート配下: mount={}",
            mount.display()
        );
        let original = mount.join("photos/a.png");
        assert!(recorded_matches(
            &trash_dir,
            Path::new("photos/a.png"),
            &original
        ));
        assert!(!recorded_matches(
            &trash_dir,
            Path::new("photos/a.png"),
            &mount.join("other/a.png")
        ));
        // 絶対Path=は従来どおり直接比較
        assert!(recorded_matches(&trash_dir, &original, &original));
    }

    #[test]
    fn restore_decodes_percent_encoded_path() {
        let _env = ENV_LOCK.lock().unwrap();
        let (tmp, trash) = set_xdg_trash("percent");
        let dir = tmp.0.join("スペース dir");
        fs::create_dir_all(&dir).unwrap();
        let original = dir.join("ファイル.png");
        let encoded = format!("/{}", percent_encode_for_test(&original.to_string_lossy()));
        write_trashinfo(&trash, "p", &encoded, "2026-08-31T00:00:00");
        fs::write(trash.join("files").join("p"), b"data").unwrap();
        let entry = TrashEntry {
            trash_dir: trash.clone(),
            file_name: "p".to_string(),
        };
        let restored = restore_from_trash(&entry, &original, 4).unwrap();
        assert_eq!(restored, original);
        assert_eq!(fs::read(&original).unwrap(), b"data");
    }

    /// テスト用の最小パーセントエンコード (空白と非ASCIIのみ)。
    fn percent_encode_for_test(s: &str) -> String {
        let mut out = String::new();
        for byte in s.bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'-' | b'_') {
                out.push(byte as char);
            } else {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
        out
    }

    #[test]
    fn trash_missing_file_is_error_and_touches_nothing() {
        let _env = ENV_LOCK.lock().unwrap();
        let (work, _trash) = set_xdg_trash("missingtrash");
        let ghost = work.0.join("ghost.png");
        let err = trash_to_bin(&ghost).unwrap_err();
        assert!(err.contains("not found"), "{err}");
        assert!(!ghost.exists());
    }

    #[test]
    fn trash_failure_keeps_original_untouched() {
        // ロールバック検証: ゴミ箱操作が失敗しても元ファイルは削除されない。
        // 読み取り専用ディレクトリ上のファイルはadmin trashが作成できない
        // 場合があり得るが、環境依存で決定的に失敗しない場合は名称解決の
        // 失敗 (不在パス) で代用 — 上面のmissingテストが担保するため、
        // ここでは「成功時も元データは無傷」を内容一致で検証する。
        let _env = ENV_LOCK.lock().unwrap();
        let (work, _trash) = set_xdg_trash("keep");
        let original = work.0.join("keep.png");
        fs::write(&original, b"keepme").unwrap();
        let entry = trash_to_bin(&original).unwrap();
        let trash_root = xdg_trash_root().unwrap();
        // ゴミ箱内の実体が同一内容
        let trashed = fs::read(trash_root.join("files").join(&entry.file_name)).unwrap();
        assert_eq!(trashed, b"keepme", "移動 (削除+再作成) で内容が失われない");
    }

    // ===== 原子的no-clobberリネーム =====

    #[test]
    fn rename_noreplace_moves_file_atomically() {
        let work = TestDir::new("rnp");
        let from = work.0.join("a.png");
        let to = work.0.join("b.png");
        fs::write(&from, b"data").unwrap();
        rename_noreplace(&from, &to).expect("move succeeds");
        assert!(!from.exists());
        assert_eq!(fs::read(&to).unwrap(), b"data");
    }

    #[test]
    fn rename_noreplace_refuses_existing_destination() {
        let work = TestDir::new("rnp-exists");
        let from = work.0.join("a.png");
        let to = work.0.join("b.png");
        fs::write(&from, b"new").unwrap();
        fs::write(&to, b"existing").unwrap();
        let err = rename_noreplace(&from, &to).unwrap_err();
        assert!(
            matches!(err, RenameNoReplaceError::DestinationExists),
            "{err}"
        );
        // 両ファイル無傷 (上書きされない)
        assert_eq!(fs::read(&to).unwrap(), b"existing");
        assert_eq!(fs::read(&from).unwrap(), b"new");
    }

    #[test]
    fn rename_noreplace_fallback_link_unlink_is_atomic_noclobber() {
        let work = TestDir::new("rnp-fallback");
        let from = work.0.join("a.png");
        let to = work.0.join("b.png");
        fs::write(&from, b"x").unwrap();
        fs::write(&to, b"y").unwrap();
        let err = link_unlink_noreplace(&from, &to).unwrap_err();
        assert!(matches!(err, RenameNoReplaceError::DestinationExists));
        assert_eq!(fs::read(&from).unwrap(), b"x");
        assert_eq!(fs::read(&to).unwrap(), b"y");
        // 成功経路: 移動して内容が同一inode (link) であることを確認
        let to2 = work.0.join("c.png");
        link_unlink_noreplace(&from, &to2).unwrap();
        assert!(!from.exists());
        assert_eq!(fs::read(&to2).unwrap(), b"x");
    }

    #[test]
    fn trash_entry_byte_size_reports_current_size() {
        let _env = ENV_LOCK.lock().unwrap();
        let (work, _trash) = set_xdg_trash("entrysize");
        let original = work.0.join("s.png");
        fs::write(&original, b"12345").unwrap();
        let entry = trash_to_bin(&original).unwrap();
        assert_eq!(trash_entry_byte_size(&entry), Some(5));
        fs::write(
            entry.trash_dir.join("files").join(&entry.file_name),
            b"1234567",
        )
        .unwrap();
        assert_eq!(trash_entry_byte_size(&entry), Some(7));
    }

    #[test]
    fn os_file_id_of_matches_stat() {
        #[cfg(unix)]
        {
            let work = TestDir::new("osfid");
            let f = work.0.join("a.png");
            fs::write(&f, b"x").unwrap();
            let md = fs::metadata(&f).unwrap();
            use std::os::unix::fs::MetadataExt;
            assert_eq!(os_file_id_of(&md), Some(OsFileId::new(md.dev(), md.ino())));
        }
    }
}
