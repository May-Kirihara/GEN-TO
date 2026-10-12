//! フォルダツリーとOS連携のcommand群 (MVP WP-4)。
//!
//! - `fs_list_dirs`: 任意パスの直下ディレクトリ列挙 (遅延ロード用)。
//!   起点はユーザーホームとプラットフォームのルート群 (Windowsは
//!   存在するドライブ、それ以外は /)。隠しディレクトリ (`.` 始まり) は
//!   表示しない (設定はWP-15)。権限エラーはエラー情報として返し、UIが
//!   「読み取り不能」ノードを表示する。
//! - `reveal_in_file_manager` / `open_with_default_app`: tauri-plugin-opener
//!   のRust APIを呼ぶ薄いcommand。JS側からplugin commandを直接叩かないため
//!   capabilityへの権限追加は不要 (WP-4依存メモ参照)。パスの存在検証は
//!   この層で行う (プロトコルと同じく「明示的な対象のみ」原則)。

use std::path::{Path, PathBuf};

use serde::Serialize;

/// `fs_list_dirs` のディレクトリ項目DTO。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FsDirEntryDto {
    pub name: String,
    pub path: String,
}

/// `fs_list_dirs` の応答DTO。
///
/// `error` が非nullの場合は entries が空でもエラー状態 (UIはノードを
/// 「読み取り不能」等として表示)。ルート要求 (`path: null`) の場合は
/// 起点ノード列 (ホーム + プラットフォームのルート群) を返し、`path` は
/// 空文字。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FsDirListDto {
    pub path: String,
    pub parent: Option<String>,
    pub entries: Vec<FsDirEntryDto>,
    pub error: Option<String>,
}

/// ディレクトリ列挙の失敗種別 (UI表示分けに使う)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListDirsError {
    NotFound,
    NotADirectory,
    Unreadable(String),
}

impl ListDirsError {
    fn message(&self) -> String {
        match self {
            ListDirsError::NotFound => "フォルダが見つかりません".to_string(),
            ListDirsError::NotADirectory => "フォルダではありません".to_string(),
            ListDirsError::Unreadable(reason) => format!("読み取りできません ({reason})"),
        }
    }
}

struct DirEntryInfo {
    name: String,
    path: PathBuf,
}

/// 隠しディレクトリ判定 (Unix慣習: `.` 始まり。WP-15で設定化)。
fn is_hidden_name(name: &str) -> bool {
    name.starts_with('.')
}

/// 直下ディレクトリ列挙のコア。シンボリックリンクはリンク先がディレクトリ
/// なら含める (fs::metadataはリンクを追従する)。壊れたリンクは除外。
pub fn list_subdirs(dir: &Path) -> Result<Vec<(String, PathBuf)>, ListDirsError> {
    use std::io::ErrorKind;
    let meta = std::fs::metadata(dir).map_err(|err| match err.kind() {
        ErrorKind::NotFound => ListDirsError::NotFound,
        ErrorKind::NotADirectory => ListDirsError::NotADirectory,
        ErrorKind::PermissionDenied => ListDirsError::Unreadable("権限がありません".into()),
        _ => ListDirsError::Unreadable(err.to_string()),
    })?;
    if !meta.is_dir() {
        return Err(ListDirsError::NotADirectory);
    }
    let reader = std::fs::read_dir(dir).map_err(|err| match err.kind() {
        ErrorKind::PermissionDenied => ListDirsError::Unreadable("権限がありません".into()),
        _ => ListDirsError::Unreadable(err.to_string()),
    })?;
    let mut entries: Vec<DirEntryInfo> = Vec::new();
    for entry in reader {
        let entry = match entry {
            Ok(entry) => entry,
            // 個別エントリの読み取り失敗はその項目だけ諦める (§6 エラー隔離)
            Err(_) => continue,
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_hidden_name(&name) {
            continue;
        }
        let is_dir = entry.path().metadata().map(|m| m.is_dir()).unwrap_or(false);
        if is_dir {
            entries.push(DirEntryInfo {
                name,
                path: entry.path(),
            });
        }
    }
    // 大文字小文字を無視した名前順 (ファイラー慣習)。完全同一ならフルパスで決定論化
    entries.sort_by(|a, b| {
        let key = |e: &DirEntryInfo| e.name.to_lowercase();
        key(a).cmp(&key(b)).then_with(|| a.path.cmp(&b.path))
    });
    Ok(entries.into_iter().map(|e| (e.name, e.path)).collect())
}

fn to_dto_list(path: &Path, result: Result<Vec<(String, PathBuf)>, ListDirsError>) -> FsDirListDto {
    let parent = path.parent().map(|p| p.to_string_lossy().into_owned());
    match result {
        Ok(entries) => FsDirListDto {
            path: path.to_string_lossy().into_owned(),
            parent: if parent.as_deref() == Some("") {
                None
            } else {
                parent
            },
            entries: entries
                .into_iter()
                .map(|(name, path)| FsDirEntryDto {
                    name,
                    path: path.to_string_lossy().into_owned(),
                })
                .collect(),
            error: None,
        },
        Err(err) => FsDirListDto {
            path: path.to_string_lossy().into_owned(),
            parent: if parent.as_deref() == Some("") {
                None
            } else {
                parent
            },
            entries: Vec::new(),
            error: Some(err.message()),
        },
    }
}

/// ルート一覧 (ホーム + プラットフォームのルート群) のDTO。ホームが
/// 解決できない環境ではルート群のみ。
fn root_list(home: Option<&Path>) -> FsDirListDto {
    let mut entries = Vec::new();
    if let Some(home) = home {
        if home.is_dir() {
            entries.push(FsDirEntryDto {
                name: "ホーム".to_string(),
                path: home.to_string_lossy().into_owned(),
            });
        }
    }
    entries.extend(platform_roots());
    FsDirListDto {
        path: String::new(),
        parent: None,
        entries,
        error: None,
    }
}

/// ホーム以外の起点ノード (Windows以外): ルート(/) のみ。
#[cfg(not(windows))]
fn platform_roots() -> Vec<FsDirEntryDto> {
    vec![FsDirEntryDto {
        name: "ファイルシステム (/)".to_string(),
        path: "/".to_string(),
    }]
}

/// ホーム以外の起点ノード (Windows): 存在するドライブルートを字母順
/// (A→Z) に列挙。A:\〜Z:\ を is_dir() でプローブするため、ドライブ文字
/// が割り当てられていない・リムーバブルメディア不在のドライブは自然に
/// 除外される (依存追加禁止のためWin32 APIのドライブ列挙は使わない)。
#[cfg(windows)]
fn platform_roots() -> Vec<FsDirEntryDto> {
    (b'A'..=b'Z')
        .map(|letter| format!("{}:\\", letter as char))
        .filter(|root| Path::new(root).is_dir())
        .map(|root| FsDirEntryDto {
            name: root.clone(),
            path: root,
        })
        .collect()
}

/// 任意パスの直下ディレクトリ列挙 (遅延ロード)。`path: null` で起点ノード。
#[tauri::command]
pub fn fs_list_dirs(path: Option<String>) -> FsDirListDto {
    let Some(path) = path else {
        return root_list(dirs::home_dir().as_deref());
    };
    let path = PathBuf::from(path);
    to_dto_list(&path, list_subdirs(&path))
}

/// OSのシェル連携対象として妥当なパスか検証する (存在する実ファイル/実体)。
fn validate_existing_path(raw: &str) -> Result<PathBuf, String> {
    let path = Path::new(raw);
    let meta = std::fs::metadata(path)
        .map_err(|err| format!("パスにアクセスできません: {raw} ({err})"))?;
    if !meta.is_file() && !meta.is_dir() {
        return Err(format!("対象が見つかりません: {raw}"));
    }
    Ok(path.to_path_buf())
}

/// OSのファイルマネージャーで表示 (項目選択付きreveal)。
#[tauri::command]
pub async fn reveal_in_file_manager(path: String) -> Result<(), String> {
    let path = validate_existing_path(&path)?;
    tauri_plugin_opener::reveal_item_in_dir(&path).map_err(|err| err.to_string())
}

/// 既定の外部アプリで開く。
#[tauri::command]
pub async fn open_with_default_app(path: String) -> Result<(), String> {
    let path = validate_existing_path(&path)?;
    tauri_plugin_opener::open_path(&path, None::<&str>).map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gento-fsview-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp root");
        dir
    }

    #[test]
    fn lists_only_visible_subdirectories_sorted_case_insensitive() {
        let root = temp_root("sort");
        std::fs::create_dir_all(root.join("Beta")).unwrap();
        std::fs::create_dir_all(root.join("alpha")).unwrap();
        std::fs::create_dir_all(root.join(".hidden")).unwrap();
        std::fs::write(root.join("file.txt"), b"x").unwrap();
        std::fs::create_dir_all(root.join("Nested/Child")).unwrap();

        let entries = list_subdirs(&root).expect("list");
        let names: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["alpha", "Beta", "Nested"]);
        // 子のパスは絶対 (親パス + 名前)
        assert_eq!(entries[0].1, root.join("alpha"));
    }

    #[test]
    fn includes_symlinked_directory_but_not_broken_link() {
        let root = temp_root("symlink");
        std::fs::create_dir_all(root.join("real")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("no-such-target"), root.join("broken")).unwrap();

        let entries = list_subdirs(&root).expect("list");
        let names: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"link"), "{names:?}");
        assert!(!names.contains(&"broken"), "{names:?}");
    }

    #[test]
    fn error_kinds_for_missing_and_file_paths() {
        let root = temp_root("errors");
        assert_eq!(
            list_subdirs(&root.join("no-such")),
            Err(ListDirsError::NotFound)
        );
        let file = root.join("f.txt");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(list_subdirs(&file), Err(ListDirsError::NotADirectory));
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_directory_reports_permission_error() {
        // root実行ではchmodが効かないためスキップ (コンテナ内root考慮)
        if !nix_is_user() {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let root = temp_root("perm");
        let locked = root.join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let result = list_subdirs(&locked);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            matches!(result, Err(ListDirsError::Unreadable(_))),
            "{result:?}"
        );
    }

    #[cfg(unix)]
    fn nix_is_user() -> bool {
        // 簡易判定: /root へのアクセス可否ではなく実uid (libcに直依存しない)
        std::fs::read_dir("/proc/self").is_ok() && {
            let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
            !status
                .lines()
                .any(|l| l.starts_with("Uid:") && l.split_whitespace().nth(1) == Some("0"))
        }
    }

    #[test]
    fn dto_carries_error_and_parent() {
        let root = temp_root("dto");
        std::fs::create_dir_all(root.join("child")).unwrap();
        let dto = to_dto_list(&root, list_subdirs(&root));
        assert_eq!(dto.error, None);
        assert_eq!(dto.entries.len(), 1);
        assert_eq!(dto.entries[0].name, "child");
        assert_eq!(
            dto.parent.as_deref(),
            Some(root.parent().unwrap().to_str().unwrap())
        );

        // ルート (/) の親はnull扱い
        let dto_root = to_dto_list(Path::new("/"), list_subdirs(Path::new("/")));
        assert_eq!(dto_root.parent, None);

        let missing = to_dto_list(&root.join("gone"), list_subdirs(&root.join("gone")));
        assert!(missing.error.is_some());
        assert_eq!(missing.entries.len(), 0);
    }

    // Windowsの起点ノード列はホーム+ドライブ群 (下のwindows専用テストで検証)
    #[cfg(not(windows))]
    #[test]
    fn root_list_contains_home_and_filesystem() {
        let home = dirs::home_dir();
        let dto = root_list(home.as_deref());
        let names: Vec<&str> = dto.entries.iter().map(|e| e.name.as_str()).collect();
        if let Some(home) = home {
            assert_eq!(names, vec!["ホーム", "ファイルシステム (/)"]);
            assert_eq!(dto.entries[0].path, home.to_string_lossy());
        } else {
            assert_eq!(names, vec!["ファイルシステム (/)"]);
        }
        assert_eq!(dto.path, "");
        assert_eq!(dto.parent, None);
    }

    #[cfg(windows)]
    #[test]
    fn root_list_contains_home_and_drive_roots() {
        let home = dirs::home_dir();
        let dto = root_list(home.as_deref());
        if let Some(home) = &home {
            assert_eq!(dto.entries.first().map(|e| e.name.as_str()), Some("ホーム"));
            assert_eq!(dto.entries[0].path, home.to_string_lossy());
        }
        let drives: Vec<&FsDirEntryDto> = dto
            .entries
            .iter()
            .skip(usize::from(home.is_some()))
            .collect();
        assert!(!drives.is_empty(), "{dto:?}");
        // ドライブは「X:\」形式でname==path、字母順 (A→Z)
        let letters: Vec<u8> = drives
            .iter()
            .map(|e| {
                assert_eq!(e.name.len(), 3, "{e:?}");
                assert_eq!(&e.name.as_bytes()[1..3], b":\\");
                assert_eq!(e.path, e.name);
                e.name.as_bytes()[0]
            })
            .collect();
        assert!(letters.windows(2).all(|w| w[0] < w[1]), "{letters:?}");
        // カレントディレクトリのドライブルートは必ず含まれる
        // (UNCパス (\\server\share) の場合は形式が異なるため検証から除外)
        let cwd = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let bytes = cwd.as_bytes();
        if bytes.len() >= 3 && bytes[1] == b':' {
            let prefix = cwd[..3].to_uppercase();
            assert!(
                drives.iter().any(|e| e.path.eq_ignore_ascii_case(&prefix)),
                "cwd drive {prefix:?} not found in {drives:?}"
            );
        }
        assert_eq!(dto.path, "");
        assert_eq!(dto.parent, None);
    }

    #[test]
    fn validate_existing_path_rejects_missing_and_accepts_real() {
        let root = temp_root("validate");
        let file = root.join("a.png");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(
            validate_existing_path(file.to_str().unwrap()),
            Ok(file.clone())
        );
        assert_eq!(
            validate_existing_path(root.to_str().unwrap()),
            Ok(root.clone())
        );
        assert!(validate_existing_path(root.join("none").to_str().unwrap()).is_err());
    }
}
