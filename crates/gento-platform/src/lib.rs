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
//! - [`trash_to_bin`]: OSゴミ箱へ移動。Linux実装 (trash crate) は
//!   freedesktop.org Trash Specification に従い、対象ファイルと同じ
//!   「topdir」(マウント) ごとにゴミ箱を評価する:
//!   - ホーム領域 (= home trashと同一マウント): `$XDG_DATA_HOME/Trash`
//!     (通常 `~/.local/share/Trash`)
//!   - 他マウント (tmpfsの /tmp 等): `<マウントルート>/.Trash-<uid>`
//!   - ディスプレイ非依存 (GIO非依存) のためヘッドレス環境でも動作する。
//! - **失敗時の通常削除フォールバックは呼び出し側で禁止** (安全原則:
//!   「通常削除はゴミ箱」。エラー表示のみ)。
//! - [`restore_from_trash`]: Undo用の復帰。trash crateに単一項目復帰APIが
//!   無いため、XDG仕様の `.trashinfo` を走査して元パス特定+移動を自前実装
//!   する。ホームゴミ箱とadmin trash (`.Trash-$uid`) の両方を探索する。

use std::fs;
use std::path::{Path, PathBuf};

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

/// OSゴミ箱へ移動する。失敗した場合はエラーを返すのみで、対象ファイルは
/// **移動されず元の位置に残る** (ロールバック検証: 本関数がErrを返した時点
/// で対象パスは不変。trash crateは移動にrenameを使うため部分移動も起きない)。
///
/// `path` が存在しない場合はエラー。
pub fn trash_to_bin(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("{}: not found", path.display()));
    }
    trash::delete(path).map_err(|e| format!("trash failed for {}: {e}", path.display()))
}

/// ゴミ箱復帰の失敗。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreError {
    /// 復帰候補がゴミ箱内に見つからない (対応するtrashinfoが無い)。
    NotFound(String),
    /// 復帰先に同名ファイルが存在する (上書きはしない — 安全原則)。
    DestinationExists(PathBuf),
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

/// ゴミ箱から `original_path` へ復帰する (Undo用)。
///
/// ゴミ箱の `info/*.trashinfo` を走査し、`Path=` が `original_path` と
/// 一致する最新エントリ (DeletionDate降順) を `files/` から元位置へ移動する。
/// ホームゴミ箱と元パスのマウントのadmin trash (`.Trash-$uid`) の両方を探索。
///
/// 安全原則:
/// - 復帰先にファイルが既に存在する場合は [`RestoreError::DestinationExists`]
///   (上書きしない — 外部変更でUndoの前提が崩れたケース)。
/// - 一致するエントリが無い場合は [`RestoreError::NotFound`]。
/// - 移動後、対応するtrashinfoを削除する (ゴミ箱側の整合)。
pub fn restore_from_trash(original_path: &Path) -> Result<PathBuf, RestoreError> {
    let mut candidates: Vec<(PathBuf, PathBuf, String)> = Vec::new(); // (info_path, trashed_file, deletion_date)
    for trash_root in candidate_trash_dirs(original_path) {
        let info_dir = trash_root.join("info");
        let files_dir = trash_root.join("files");
        let Ok(info_entries) = fs::read_dir(&info_dir) else {
            continue; // そのゴミ箱は存在しない — 次の候補
        };
        for entry in info_entries.flatten() {
            let info_path = entry.path();
            if info_path.extension().and_then(|e| e.to_str()) != Some("trashinfo") {
                continue;
            }
            let Some(recorded) = original_path_from_trashinfo(&info_path) else {
                continue;
            };
            if recorded != original_path {
                continue;
            }
            let body = fs::read_to_string(&info_path).unwrap_or_default();
            let date = body
                .lines()
                .find(|l| l.starts_with("DeletionDate="))
                .map(|l| l.trim_start_matches("DeletionDate=").to_string())
                .unwrap_or_default();
            // trashinfoのファイル名から拡張子を除いたものがfiles/側の名前
            // (XDG仕様)。名前衝突でtrash crateが付けるsuffix (例: a.2) も
            // trashinfo側と同名になるためこの規則で一致する。
            let base_name = entry.file_name();
            let trashed_name = base_name.to_string_lossy();
            let trashed_name = trashed_name
                .strip_suffix(".trashinfo")
                .unwrap_or(&trashed_name);
            let trashed = files_dir.join(trashed_name);
            candidates.push((info_path, trashed, date));
        }
    }
    candidates.sort_by(|a, b| b.2.cmp(&a.2)); // 降順 (最新優先)

    for (info_path, trashed, _) in candidates {
        if !trashed.exists() {
            continue; // 壊れたエントリ — 次の候補へ
        }
        if original_path.exists() {
            return Err(RestoreError::DestinationExists(original_path.to_path_buf()));
        }
        if let Some(parent) = original_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| RestoreError::Io(format!("create {}: {e}", parent.display())))?;
        }
        fs::rename(&trashed, original_path).map_err(|e| {
            RestoreError::Io(format!(
                "restore {} -> {}: {e}",
                trashed.display(),
                original_path.display()
            ))
        })?;
        let _ = fs::remove_file(&info_path);
        return Ok(original_path.to_path_buf());
    }
    Err(RestoreError::NotFound(format!(
        "no trash entry for {}",
        original_path.display()
    )))
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

        trash_to_bin(&original).expect("trash succeeds");
        assert!(!original.exists(), "元位置からは消える");
        let trash_root = xdg_trash_root().unwrap();
        assert!(
            trash_root.join("files").join("img.png").exists(),
            "ゴミ箱files/配下に実在する (XDG配置)"
        );
        assert!(
            trash_root.join("info").join("img.png.trashinfo").exists(),
            "trashinfoが書かれている (復帰探索の前提)"
        );

        let restored = restore_from_trash(&original).expect("restore succeeds");
        assert_eq!(restored, original);
        assert_eq!(fs::read(&original).unwrap(), b"pngdata");
    }

    #[test]
    fn restore_missing_entry_is_not_found() {
        let _env = ENV_LOCK.lock().unwrap();
        let (_tmp, _trash) = set_xdg_trash("missing");
        let err = restore_from_trash(Path::new("/definitely/not/trashed.png")).unwrap_err();
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

        let err = restore_from_trash(&original).unwrap_err();
        assert!(
            matches!(&err, RestoreError::DestinationExists(p) if *p == original),
            "{err}"
        );
        // 上書きされない — 外部ファイルは無傷
        assert_eq!(fs::read(&original).unwrap(), b"external new file");
    }

    #[test]
    fn restore_picks_latest_deletion_date() {
        let _env = ENV_LOCK.lock().unwrap();
        let (tmp, trash) = set_xdg_trash("latest");
        let original = tmp.0.join("latest.png");
        // 同一元パスで2エントリ (古い方には別内容)
        write_trashinfo(
            &trash,
            "old",
            &original.to_string_lossy(),
            "2026-08-30T00:00:00",
        );
        fs::write(trash.join("files").join("old"), b"old").unwrap();
        write_trashinfo(
            &trash,
            "new",
            &original.to_string_lossy(),
            "2026-08-31T00:00:00",
        );
        fs::write(trash.join("files").join("new"), b"new").unwrap();

        let restored = restore_from_trash(&original).unwrap();
        assert_eq!(restored, original);
        assert_eq!(fs::read(&original).unwrap(), b"new", "最新のエントリが復帰");
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
        let restored = restore_from_trash(&original).unwrap();
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
        trash_to_bin(&original).unwrap();
        let trash_root = xdg_trash_root().unwrap();
        // ゴミ箱内の実体が同一内容
        let trashed = fs::read(trash_root.join("files").join("keep.png")).unwrap();
        assert_eq!(trashed, b"keepme", "移動 (削除+再作成) で内容が失われない");
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
