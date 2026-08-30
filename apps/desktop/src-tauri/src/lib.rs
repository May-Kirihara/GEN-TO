//! GEN-TO 技術検証プロトタイプのデスクトップシェル。
//!
//! 本crateはTauri commandとプロトコル登録の薄いアダプターに限定する。
//! ビジネスロジックはgento-core / gento-imageへ置く
//! (docs/implementation-plan.md §5, §5.1)。

pub mod catalog;
pub mod protocol;
pub mod thumbnails;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};

use catalog::AppState;
use gento_core::HealthInfo;
use tauri::{Manager, RunEvent, State};
use tracing_subscriber::EnvFilter;

use crate::thumbnails::ThumbnailService;

const APP_NAME: &str = "GEN-TO 技術検証プロトタイプ";

/// SIGTERM/SIGINTハンドラが書き込むself-pipeの書き込み側。
/// ハンドラ内で安全に呼べるのはatomic読み取りとwrite(2)のみのため。
static SIGNAL_PIPE_W: AtomicI32 = AtomicI32::new(-1);

extern "C" fn on_termination_signal(_sig: libc::c_int) {
    let fd = SIGNAL_PIPE_W.load(Ordering::SeqCst);
    if fd >= 0 {
        let byte = 1u8;
        // write(2)はasync-signal-safe。失敗時の回復手段は無いので黙って諦める
        unsafe {
            let _ = libc::write(fd, &byte as *const u8 as *const libc::c_void, 1);
        }
    }
}

/// SIGTERM/SIGINTでGraceful終了する (WP-7 / フェーズ8「終了安全」)。
/// 既定のSIGTERM摂動ではプロセスが即死しRunEvent::Exitが走らないため、
/// self-pipe + 監視スレッドでapp.exit(0)へ繋ぎ、ワーカー排出と.tmp掃除を
/// 実行させる。ウィンドウ閉じでも同じExit経路になる。
fn install_termination_signals(app: tauri::AppHandle) {
    let read_fd = unsafe {
        let mut fds = [0i32; 2];
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            tracing::warn!("failed to create signal pipe; SIGTERM will not be graceful");
            return;
        }
        libc::signal(
            libc::SIGTERM,
            on_termination_signal as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGINT,
            on_termination_signal as *const () as libc::sighandler_t,
        );
        SIGNAL_PIPE_W.store(fds[1], Ordering::SeqCst);
        fds[0]
    };
    std::thread::Builder::new()
        .name("term-signal".into())
        .spawn(move || {
            let mut byte = [0u8; 1];
            let n = unsafe { libc::read(read_fd, byte.as_mut_ptr() as *mut libc::c_void, 1) };
            if n == 1 {
                tracing::info!("termination signal received; exiting for clean shutdown");
                app.exit(0);
            }
        })
        .ok();
}

#[tauri::command]
fn health(state: State<'_, AppState>) -> HealthInfo {
    let path = &state.dataset_path;
    tracing::debug!(dataset = %path.display(), "health requested");
    HealthInfo {
        app_name: APP_NAME.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        dataset_path: Some(display_absolute(path)),
        dataset_exists: path.exists(),
        timestamp_ms: epoch_ms(),
    }
}

/// 実機計測ブリッジ (devtest.ts) の有効化。既定は無効。
/// release buildでも `GENTO_DEV_BRIDGE=1` で計測を可能にする (計画 §16)。
/// フラグは AppState (起動時にenvから確定) が持ち、無効ならErrを返す
/// (危険コマンドの常時登録を登録済みでも無害化する。FG-1)。
#[tauri::command]
fn dev_bridge_enabled(state: State<'_, AppState>) -> Result<bool, String> {
    dev_bridge_enabled_impl(&state)
}

/// 計測ブリッジから機械可読な行をログへ流す (bash側は `devtest:` でgrepする)。
/// ブリッジ無効時はErr (書き込み不能)。
#[tauri::command]
fn devtest_report(state: State<'_, AppState>, payload: String) -> Result<(), String> {
    devtest_report_impl(&state, &payload)
}

/// [`dev_bridge_enabled`] のゲート付き本体 (単体テスト用にStateから切り出した)。
pub(crate) fn dev_bridge_enabled_impl(state: &AppState) -> Result<bool, String> {
    if state.dev_bridge_enabled() {
        Ok(true)
    } else {
        Err(DEV_BRIDGE_DISABLED_MESSAGE.to_string())
    }
}

/// [`devtest_report`] のゲート付き本体 (単体テスト用にStateから切り出した)。
pub(crate) fn devtest_report_impl(state: &AppState, payload: &str) -> Result<(), String> {
    dev_bridge_gate(state)?;
    tracing::info!(target: "devtest", "{payload}");
    Ok(())
}

/// 危険コマンド共通のゲート。無効ならErr。
pub(crate) fn dev_bridge_gate(state: &AppState) -> Result<(), String> {
    if state.dev_bridge_enabled() {
        Ok(())
    } else {
        Err(DEV_BRIDGE_DISABLED_MESSAGE.to_string())
    }
}

const DEV_BRIDGE_DISABLED_MESSAGE: &str =
    "dev bridge is disabled (set GENTO_DEV_BRIDGE=1 to enable)";

/// `GENTO_DEV_BRIDGE` の値が有効か。**値がちょうど `"1"` のときのみ有効**。
/// 未設定・`0`・`true`・空文字は無効 (releaseでの意図しない有効化を防ぐ)。
fn dev_bridge_value_enabled(value: Option<&std::ffi::OsStr>) -> bool {
    value == Some(std::ffi::OsStr::new("1"))
}

/// 環境変数から計測ブリッジ有効フラグを確定する (AppState生成時に一度だけ)。
pub(crate) fn dev_bridge_enabled_strict() -> bool {
    dev_bridge_value_enabled(std::env::var_os("GENTO_DEV_BRIDGE").as_deref())
}

pub fn run() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    protocol::register_uri_scheme_protocols(tauri::Builder::default())
        .setup(move |app| {
            init_tracing();
            install_termination_signals(app.handle().clone());
            let dataset_path = resolve_dataset_path(&args);
            let cache_dir = resolve_cache_dir(&args);
            tracing::info!(
                dataset = %dataset_path.display(),
                exists = dataset_path.exists(),
                "dataset resolved"
            );
            tracing::info!(cache_dir = %cache_dir.display(), "thumbnail cache dir resolved");
            let events = Box::new(thumbnails::TauriThumbEvents {
                app: app.handle().clone(),
            });
            let service = match ThumbnailService::new(cache_dir, events) {
                Ok(service) => service,
                Err(err) => {
                    // キャッシュ初期化失敗では起動を続行できない (サムネイル機能が
                    // 前提のため)。setupエラーとしてアプリを落とす。
                    tracing::error!(error = %err, "failed to open thumbnail cache");
                    return Err(err.into());
                }
            };
            let mut state = AppState::new(dataset_path);
            state.thumbnails = Some(service);
            app.manage(state);
            if app.get_webview_window("main").is_some() {
                tracing::info!("main window created");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            health,
            dev_bridge_enabled,
            devtest_report,
            catalog::get_catalog_summary,
            catalog::get_catalog_page,
            catalog::start_scan,
            catalog::set_catalog_mode,
            thumbnails::request_thumbnails,
            thumbnails::reprioritize_thumbnails,
            thumbnails::cancel_thumbnail_requests,
            thumbnails::generate_all_thumbnails,
            thumbnails::get_thumbnail_stats,
            thumbnails::pause_thumbnail_generation,
            thumbnails::clear_thumbnail_cache,
            thumbnails::set_failure_injection
        ])
        .build(tauri::generate_context!())
        .expect("failed to build tauri application")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                if let Some(state) = app.try_state::<AppState>() {
                    if let Some(service) = &state.thumbnails {
                        tracing::info!("shutting down thumbnail service");
                        service.shutdown();
                    }
                }
            }
        });
}

/// `GENTO_LOG` (tracing EnvFilter書式、既定 `info`) でstdoutへログを出す。
fn init_tracing() {
    let filter = EnvFilter::try_from_env("GENTO_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stdout)
        .init();
}

/// `--dataset <PATH>` を解析する。指定がなければCWD相対の `sample/dataset`。
fn resolve_dataset_path(args: &[String]) -> PathBuf {
    let value = flag_value(args, "--dataset");
    match value {
        Some(v) => PathBuf::from(v),
        None => PathBuf::from("sample/dataset"),
    }
}

/// `--cache-dir <PATH>` を解析する。指定がなければ
/// `dirs::cache_dir()/gento-spike/thumbnails` (不正時は一時領域へフォールバック)。
fn resolve_cache_dir(args: &[String]) -> PathBuf {
    if let Some(v) = flag_value(args, "--cache-dir") {
        return PathBuf::from(v);
    }
    let base = dirs::cache_dir().unwrap_or_else(std::env::temp_dir);
    base.join("gento-spike").join("thumbnails")
}

/// フラグ形式 `--name value` の値を取り出す。値なしの場合は既定へフォールバックしwarn。
fn flag_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == name {
            match iter.next() {
                Some(value) => return Some(value),
                None => tracing::warn!("{name} without a value; using default"),
            }
        }
    }
    None
}

/// UI表示用に相対パスを絶対パスへ解決する (存在チェックはしない)。
fn display_absolute(path: &Path) -> String {
    std::path::absolute(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_dataset_flag() {
        let args = vec!["--dataset".to_string(), "/tmp/x".to_string()];
        assert_eq!(resolve_dataset_path(&args), PathBuf::from("/tmp/x"));
    }

    #[test]
    fn falls_back_to_default_path() {
        assert_eq!(resolve_dataset_path(&[]), PathBuf::from("sample/dataset"));
        let trailing = vec!["--dataset".to_string()];
        assert_eq!(
            resolve_dataset_path(&trailing),
            PathBuf::from("sample/dataset")
        );
    }

    #[test]
    fn resolves_cache_dir_flag() {
        let args = vec!["--cache-dir".to_string(), "/tmp/mycache".to_string()];
        assert_eq!(resolve_cache_dir(&args), PathBuf::from("/tmp/mycache"));
        // datasetとの共存
        let both = vec![
            "--dataset".to_string(),
            "/data".to_string(),
            "--cache-dir".to_string(),
            "/tmp/c2".to_string(),
        ];
        assert_eq!(resolve_dataset_path(&both), PathBuf::from("/data"));
        assert_eq!(resolve_cache_dir(&both), PathBuf::from("/tmp/c2"));
    }

    #[test]
    fn default_cache_dir_is_under_user_cache() {
        let dir = resolve_cache_dir(&[]);
        let base = dirs::cache_dir().unwrap_or_else(std::env::temp_dir);
        assert_eq!(dir, base.join("gento-spike").join("thumbnails"));
    }

    // ===== 計測ブリッジのゲート (FG-1 / F2) =====

    use std::ffi::OsStr;

    #[test]
    fn dev_bridge_env_value_must_be_exactly_one() {
        // 値がちょうど "1" のときのみ有効
        assert!(dev_bridge_value_enabled(Some(OsStr::new("1"))));
        // 未設定・"0"・"true"・空文字・前後空白は無効
        assert!(!dev_bridge_value_enabled(None));
        assert!(!dev_bridge_value_enabled(Some(OsStr::new("0"))));
        assert!(!dev_bridge_value_enabled(Some(OsStr::new("true"))));
        assert!(!dev_bridge_value_enabled(Some(OsStr::new(""))));
        assert!(!dev_bridge_value_enabled(Some(OsStr::new(" 1"))));
        assert!(!dev_bridge_value_enabled(Some(OsStr::new("1\n"))));
    }

    #[test]
    fn dev_bridge_commands_gate_on_appstate_flag() {
        use thumbnails::{ThumbEvents, ThumbFailedItem, ThumbReadyItem};

        struct NoEvents;
        impl ThumbEvents for NoEvents {
            fn ready(&self, _items: &[ThumbReadyItem]) {}
            fn failed(&self, _items: &[ThumbFailedItem]) {}
        }

        let cache_root =
            std::env::temp_dir().join(format!("gento-lib-gate-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cache_root);
        let service =
            ThumbnailService::new(cache_root.clone(), Box::new(NoEvents)).expect("service");
        // テスト実行時の環境変数に依存しないようフラグを明示する
        let mut state = AppState::new(PathBuf::from("/tmp/gento-test-dataset"));
        state.thumbnails = Some(std::sync::Arc::clone(&service));
        state
            .dev_bridge
            .store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(dev_bridge_enabled_impl(&state).is_err());
        assert!(devtest_report_impl(&state, "G line").is_err());
        assert!(
            thumbnails::set_failure_injection_impl(&state, "decode", 0.5).is_err(),
            "set_failure_injection must be gated"
        );

        state
            .dev_bridge
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(dev_bridge_enabled_impl(&state), Ok(true));
        assert!(devtest_report_impl(&state, "G line").is_ok());
        let dto = thumbnails::set_failure_injection_impl(&state, "none", 0.0).unwrap();
        assert_eq!(dto.mode, "none");
        assert_eq!(dto.rate, 0.0);
        // 無効時と異なり、不正な引数は通常どおり引数検証で拒否される
        assert!(thumbnails::set_failure_injection_impl(&state, "bogus", 0.5).is_err());
        assert!(thumbnails::set_failure_injection_impl(&state, "decode", 1.5).is_err());
        service.shutdown();
        let _ = std::fs::remove_dir_all(&cache_root);
    }
}
