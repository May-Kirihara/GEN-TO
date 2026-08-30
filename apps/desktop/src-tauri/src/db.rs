//! DB最小wiring (WP-1): パス解決・状態保持・`db_status` command。
//!
//! アプリ起動時にDBを開いて [`AppState`] へ保持する。開けなかった場合も
//! アプリは続行する (スパイク由来の通常ファイラー機能はDB非依存のため。
//! WP-2以降の登録ルート機能は `db_status` のエラーを見て判断する)。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use gento_core::AppDirs;
use serde::Serialize;
use tauri::State;

use crate::catalog::AppState;

/// DBの稼働状態。setupで一度確定し以後不変。
pub struct DbState {
    /// 解決済みDBファイルパス。
    pub path: PathBuf,
    /// 開けたDB。開けなければ `None`。
    pub db: Option<Arc<gento_db::Db>>,
    /// 開けなかった場合のエラー表示。
    pub open_error: Option<String>,
}

impl DbState {
    /// DBを開く (親ディレクトリ作成・migration含む)。失敗してもpanicせず
    /// エラーを保持した状態を返す (アプリ継続)。
    pub fn open(path: PathBuf) -> Self {
        match gento_db::Db::open(&path) {
            Ok(db) => Self {
                path,
                db: Some(Arc::new(db)),
                open_error: None,
            },
            Err(err) => {
                tracing::error!(error = %err, path = %path.display(), "failed to open database");
                Self {
                    open_error: Some(sanitize_error_message(&err.to_string(), &path)),
                    path,
                    db: None,
                }
            }
        }
    }
}

/// `db_status` のエラー文字列から絶対パスを伏せる。エラー種別
/// (開けなかった理由・失敗した操作) はそのまま返す。
///
/// DBパス自体は `DbStatusDto.path` で露出を許容する — MVPのwebviewは
/// ローカルアプリの信頼されたコンテンツであり、ユーザー自身の環境の
/// パスだから (リモート/非信頼コンテンツを載せる前に再検討すること)。
/// エラー文中のパスはトラブルシュートに不要な複製のため伏せる。
fn sanitize_error_message(msg: &str, db_path: &std::path::Path) -> String {
    let mut sanitized = msg.replace(&db_path.display().to_string(), "<db>");
    if let Some(parent) = db_path.parent() {
        if !parent.as_os_str().is_empty() {
            sanitized = sanitized.replace(&parent.display().to_string(), "<path>");
        }
    }
    sanitized
}

/// DBパスの解決優位順: `--db <PATH>` 起動引数 > `GENTO_DB` 環境変数 >
/// `AppDirs.data_dir/library.sqlite3` (既定)。上書きはテスト・計測用。
pub fn resolve_db_path(args: &[String], app_dirs: &AppDirs) -> PathBuf {
    if let Some(v) = crate::flag_value(args, "--db") {
        return PathBuf::from(v);
    }
    if let Ok(v) = std::env::var("GENTO_DB") {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    app_dirs.data_dir.join(gento_db::DB_FILE_NAME)
}

/// `db_status` commandの応答DTO。DB未開時は統計系フィールドが `null`。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DbStatusDto {
    pub path: String,
    pub open: bool,
    pub open_error: Option<String>,
    pub schema_version: Option<i32>,
    pub journal_mode: Option<String>,
    /// `ok` または問題の要約。
    pub integrity_check: Option<String>,
    pub foreign_key_violations: Option<u64>,
    pub table_row_counts: Option<BTreeMap<String, u64>>,
    pub db_bytes: Option<u64>,
    pub wal_bytes: Option<u64>,
    pub busy_timeout_ms: Option<u64>,
}

#[tauri::command]
pub fn db_status(state: State<'_, AppState>) -> DbStatusDto {
    let Some(db_state) = &state.db else {
        // setupを通らない文脈 (単体テスト) でのみ到達し得る
        return DbStatusDto {
            path: String::new(),
            open: false,
            open_error: Some("database state not initialized".to_string()),
            schema_version: None,
            journal_mode: None,
            integrity_check: None,
            foreign_key_violations: None,
            table_row_counts: None,
            db_bytes: None,
            wal_bytes: None,
            busy_timeout_ms: None,
        };
    };
    let Some(db) = &db_state.db else {
        return DbStatusDto {
            path: db_state.path.display().to_string(),
            open: false,
            open_error: db_state.open_error.clone(),
            schema_version: None,
            journal_mode: None,
            integrity_check: None,
            foreign_key_violations: None,
            table_row_counts: None,
            db_bytes: None,
            wal_bytes: None,
            busy_timeout_ms: None,
        };
    };
    match db.diagnose() {
        Ok(status) => DbStatusDto {
            path: status.path,
            open: true,
            open_error: None,
            schema_version: Some(status.schema_version),
            journal_mode: Some(status.journal_mode),
            integrity_check: Some(status.integrity_check),
            foreign_key_violations: Some(status.foreign_key_violations),
            table_row_counts: Some(status.table_row_counts),
            db_bytes: Some(status.db_bytes),
            wal_bytes: Some(status.wal_bytes),
            busy_timeout_ms: Some(status.busy_timeout_ms),
        },
        // 開いているが診断失敗 (ディスク障害等)。open=true + エラー種別
        // (絶対パスは伏せる)。
        Err(err) => DbStatusDto {
            path: db_state.path.display().to_string(),
            open: true,
            open_error: Some(sanitize_error_message(&err.to_string(), &db_state.path)),
            schema_version: None,
            journal_mode: None,
            integrity_check: None,
            foreign_key_violations: None,
            table_row_counts: None,
            db_bytes: None,
            wal_bytes: None,
            busy_timeout_ms: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GENTO_DB環境変数を扱うテストの直列化 (環境変数はプロセスグローバル
    /// ため、set/remove中の並行resolve_db_path読み取りが競合する)。
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn app_dirs() -> AppDirs {
        AppDirs {
            data_dir: PathBuf::from("/xdg/data/gento"),
            cache_dir: PathBuf::from("/xdg/cache/gento"),
            log_dir: PathBuf::from("/xdg/cache/gento/log"),
        }
    }

    #[test]
    fn default_db_path_is_under_data_dir() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("GENTO_DB");
        let dirs = app_dirs();
        assert_eq!(
            resolve_db_path(&[], &dirs),
            PathBuf::from("/xdg/data/gento/library.sqlite3")
        );
    }

    #[test]
    fn db_flag_overrides_default() {
        let dirs = app_dirs();
        let args = vec!["--db".to_string(), "/tmp/measure.sqlite3".to_string()];
        assert_eq!(
            resolve_db_path(&args, &dirs),
            PathBuf::from("/tmp/measure.sqlite3")
        );
        // 他フラグとの共存
        let both = vec![
            "--dataset".to_string(),
            "/data".to_string(),
            "--db".to_string(),
            "/tmp/x.sqlite3".to_string(),
        ];
        assert_eq!(
            resolve_db_path(&both, &dirs),
            PathBuf::from("/tmp/x.sqlite3")
        );
    }

    #[test]
    fn env_var_overrides_default_but_flag_wins() {
        // 注意: 環境変数はプロセスグローバル。ENV_LOCKで直列化する
        // (default_db_path_is_under_data_dir がresolve_db_pathを読むため)。
        let _guard = ENV_LOCK.lock().unwrap();
        let dirs = app_dirs();
        std::env::set_var("GENTO_DB", "/tmp/env-db.sqlite3");
        assert_eq!(
            resolve_db_path(&[], &dirs),
            PathBuf::from("/tmp/env-db.sqlite3"),
            "環境変数が既定を上書き"
        );
        let args = vec!["--db".to_string(), "/tmp/flag.sqlite3".to_string()];
        assert_eq!(
            resolve_db_path(&args, &dirs),
            PathBuf::from("/tmp/flag.sqlite3"),
            "引数が環境変数より優先"
        );
        std::env::remove_var("GENTO_DB");

        // 空文字の環境変数は無視
        std::env::set_var("GENTO_DB", "");
        assert_eq!(
            resolve_db_path(&[], &dirs),
            PathBuf::from("/xdg/data/gento/library.sqlite3")
        );
        std::env::remove_var("GENTO_DB");
    }

    #[test]
    fn db_state_opens_temp_db_and_reports_healthy() {
        let dir = std::env::temp_dir().join(format!(
            "gento-dbstate-{}-{}",
            std::process::id(),
            gento_db::LATEST_VERSION
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("library.sqlite3");
        let state = DbState::open(path.clone());
        assert!(state.db.is_some(), "開ける: {:?}", state.open_error);
        assert!(state.open_error.is_none());
        assert!(path.exists(), "親ディレクトリ込みで作成される");
        let db = state.db.as_ref().unwrap();
        assert_eq!(db.schema_version().unwrap(), gento_db::LATEST_VERSION);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn db_state_keeps_error_when_path_is_a_directory() {
        // ディレクトリをDBパスに指定 → 開けないがpanicせずエラーを保持
        // (アプリ継続の設計)。Linuxではディレクトリへのopenは失敗する。
        let dir = std::env::temp_dir().join(format!("gento-dbstate-dir-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let state = DbState::open(dir.clone());
        assert!(state.db.is_none(), "ディレクトリはDBとして開けない");
        let err = state.open_error.expect("エラー詳細を保持");
        assert!(!err.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sanitize_error_message_replaces_paths_but_keeps_kind() {
        // DBパス本体
        let msg = sanitize_error_message(
            "failed to open database: open /xdg/data/gento/library.sqlite3: unable to open database file",
            &PathBuf::from("/xdg/data/gento/library.sqlite3"),
        );
        assert!(!msg.contains("/xdg"), "{msg}");
        assert!(msg.contains("<db>"), "{msg}");
        assert!(msg.contains("unable to open database file"), "{msg}");

        // 親ディレクトリ (backup先などDB以外のパスが乗る場合)
        let msg = sanitize_error_message(
            "database migration failed: io error: create /xdg/data/gento/db-backups failed",
            &PathBuf::from("/xdg/data/gento/library.sqlite3"),
        );
        assert!(!msg.contains("/xdg"), "{msg}");
        assert!(msg.contains("<path>"), "{msg}");
        assert!(
            msg.contains("db-backups"),
            "末端のディレクトリ名は種別情報として保持: {msg}"
        );
    }

    #[test]
    fn open_error_does_not_expose_absolute_paths() {
        // エラー種別 (開けない理由) は返すが、エラー文字列中の絶対パスは
        // 伏せる (webviewに環境情報を渡さない)。
        let dir =
            std::env::temp_dir().join(format!("gento-dbstate-sanitize-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let state = DbState::open(dir.clone());
        assert!(state.db.is_none());
        let err = state.open_error.expect("エラー詳細を保持");
        assert!(
            !err.contains(&dir.display().to_string()),
            "DBパスが露出: {err}"
        );
        assert!(
            !err.contains(&std::env::temp_dir().display().to_string()),
            "親ディレクトリが露出: {err}"
        );
        assert!(!err.is_empty(), "種別は保持される");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
