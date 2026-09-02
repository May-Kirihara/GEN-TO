//! GEN-TO デスクトップシェル (スパイクからMVP基盤へ移行、WP-0)。
//!
//! 本crateはTauri commandとプロトコル登録の薄いアダプターに限定する。
//! ビジネスロジックはgento-core / gento-imageへ置く。
//!
//! devtools境界: 計測・検証専用機能 (dev bridge・障害注入・仮想カタログ) は
//! `devtools` module配下で `debug_assertions` または `--features devtools`
//! のときのみコンパイルされる。通常releaseビルドからは完全に除去される。

pub mod catalog;
pub mod collections;
pub mod db;
#[cfg(any(debug_assertions, feature = "devtools"))]
pub mod devtools;
pub mod fileops;
pub mod fsview;
pub mod library;
pub mod metadata;
pub mod protocol;
pub mod search;
pub mod thumbnails;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};

use catalog::AppState;
use gento_core::{AppDirs, HealthInfo, GENTO_API_VERSION};
use tauri::{Manager, RunEvent, State};
use tracing_subscriber::EnvFilter;

use crate::thumbnails::ThumbnailService;

const APP_NAME: &str = "GEN-TO 技術検証プロトタイプ";

/// アプリケーション直下のディレクトリ名 (XDGベース直下に置く)。
/// スパイクからの互換で "gento-spike" を維持している。
const DIR_NAMESPACE: &str = "gento-spike";

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

/// panic時にログファイルへ診断行 (panic情報 + バージョン + OS + APIバージョン)
/// を追記するフック (WP-0)。軽量に保つためbacktraceは取らない。
/// 既定フック (stderr出力) は維持する。
fn install_panic_hook(app_dirs: &AppDirs) {
    let log_dir = app_dirs.log_dir.clone();
    let log_path = log_dir.join("panic.log");
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "<non-string panic payload>".to_string()
        };
        let line = format!(
            "panic version={} api_version={} os={} location={} payload={}\n",
            env!("CARGO_PKG_VERSION"),
            GENTO_API_VERSION,
            std::env::consts::OS,
            info.location().map(|l| l.to_string()).unwrap_or_default(),
            payload,
        );
        // ログ出力失敗がpanicをさらに誘発しないよう黙って諦める
        let _ = std::fs::create_dir_all(&log_dir);
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
        previous(info);
    }));
}

#[tauri::command]
fn health(state: State<'_, AppState>) -> HealthInfo {
    let path = state.current_folder();
    tracing::debug!(dataset = %path.display(), "health requested");
    HealthInfo {
        app_name: APP_NAME.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        api_version: GENTO_API_VERSION,
        dataset_path: Some(display_absolute(&path)),
        dataset_exists: path.exists(),
        db_open: state.db.as_ref().is_some_and(|d| d.db.is_some()),
        timestamp_ms: epoch_ms(),
    }
}

pub fn run() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let builder = protocol::register_uri_scheme_protocols(tauri::Builder::default());
    // devtoolsビルドのみ計測用commandを登録する。通常releaseビルドでは
    // invoke handlerに識別子が存在しない (release-smoke.shで検証)。
    #[cfg(any(debug_assertions, feature = "devtools"))]
    let builder = builder.invoke_handler(tauri::generate_handler![
        health,
        db::db_status,
        devtools::dev_bridge_enabled,
        devtools::devtest_report,
        catalog::get_catalog_summary,
        catalog::get_catalog_page,
        catalog::start_scan,
        catalog::cancel_scan,
        catalog::set_catalog_mode,
        catalog::set_catalog_source,
        catalog::set_catalog_sort,
        catalog::open_folder,
        library::library_roots_list,
        library::library_roots_add,
        library::library_roots_remove,
        library::library_roots_set_enabled,
        library::scan_start,
        library::scan_stop,
        library::scan_status,
        metadata::asset_set_rating,
        metadata::asset_set_cull,
        metadata::asset_set_favorite,
        metadata::asset_tags_add,
        metadata::asset_tags_remove,
        metadata::asset_tags_list,
        metadata::tag_suggest,
        metadata::tag_list,
        metadata::tag_delete,
        metadata::asset_caption_get,
        metadata::asset_caption_set,
        metadata::asset_detail,
        metadata::asset_metadata_get,
        metadata::metadata_reparse,
        collections::collections_list,
        collections::collection_create,
        collections::collection_rename,
        collections::collection_delete,
        collections::collection_add_items,
        collections::collection_remove_item,
        collections::collection_reorder,
        collections::collection_items,
        collections::collection_set_item_note,
        collections::smart_collection_create,
        collections::smart_collection_update,
        collections::smart_collection_items,
        collections::smart_collection_refresh,
        collections::collection_show,
        search::search_run,
        search::search_refresh,
        search::search_evidence,
        search::fts_integrity,
        search::fts_rebuild,
        search::fts_repair,
        fsview::fs_list_dirs,
        fsview::reveal_in_file_manager,
        fsview::open_with_default_app,
        fileops::file_ops_plan,
        fileops::file_ops_execute,
        fileops::file_ops_undo_check,
        fileops::file_ops_undo,
        fileops::file_ops_history,
        fileops::file_ops_cancel,
        fileops::folder_create,
        thumbnails::request_thumbnails,
        thumbnails::reprioritize_thumbnails,
        thumbnails::cancel_thumbnail_requests,
        thumbnails::generate_all_thumbnails,
        thumbnails::get_thumbnail_stats,
        thumbnails::pause_thumbnail_generation,
        thumbnails::clear_thumbnail_cache,
        thumbnails::set_failure_injection
    ]);
    #[cfg(not(any(debug_assertions, feature = "devtools")))]
    let builder = builder.invoke_handler(tauri::generate_handler![
        health,
        db::db_status,
        catalog::get_catalog_summary,
        catalog::get_catalog_page,
        catalog::start_scan,
        catalog::cancel_scan,
        catalog::set_catalog_source,
        catalog::set_catalog_sort,
        catalog::open_folder,
        library::library_roots_list,
        library::library_roots_add,
        library::library_roots_remove,
        library::library_roots_set_enabled,
        library::scan_start,
        library::scan_stop,
        library::scan_status,
        metadata::asset_set_rating,
        metadata::asset_set_cull,
        metadata::asset_set_favorite,
        metadata::asset_tags_add,
        metadata::asset_tags_remove,
        metadata::asset_tags_list,
        metadata::tag_suggest,
        metadata::tag_list,
        metadata::tag_delete,
        metadata::asset_caption_get,
        metadata::asset_caption_set,
        metadata::asset_detail,
        metadata::asset_metadata_get,
        metadata::metadata_reparse,
        collections::collections_list,
        collections::collection_create,
        collections::collection_rename,
        collections::collection_delete,
        collections::collection_add_items,
        collections::collection_remove_item,
        collections::collection_reorder,
        collections::collection_items,
        collections::collection_set_item_note,
        collections::smart_collection_create,
        collections::smart_collection_update,
        collections::smart_collection_items,
        collections::smart_collection_refresh,
        collections::collection_show,
        search::search_run,
        search::search_refresh,
        search::search_evidence,
        search::fts_integrity,
        search::fts_rebuild,
        search::fts_repair,
        fsview::fs_list_dirs,
        fsview::reveal_in_file_manager,
        fsview::open_with_default_app,
        fileops::file_ops_plan,
        fileops::file_ops_execute,
        fileops::file_ops_undo_check,
        fileops::file_ops_undo,
        fileops::file_ops_history,
        fileops::file_ops_cancel,
        fileops::folder_create,
        thumbnails::request_thumbnails,
        thumbnails::reprioritize_thumbnails,
        thumbnails::cancel_thumbnail_requests,
        thumbnails::generate_all_thumbnails,
        thumbnails::get_thumbnail_stats,
        thumbnails::pause_thumbnail_generation,
        thumbnails::clear_thumbnail_cache
    ]);
    builder
        .setup(move |app| {
            let app_dirs = resolve_app_dirs();
            init_tracing();
            install_panic_hook(&app_dirs);
            install_termination_signals(app.handle().clone());
            let dataset_path = resolve_dataset_path(&args);
            let thumbnail_root = resolve_thumbnail_root(&args, &app_dirs);
            tracing::info!(
                data_dir = %app_dirs.data_dir.display(),
                cache_dir = %app_dirs.cache_dir.display(),
                log_dir = %app_dirs.log_dir.display(),
                "app dirs resolved"
            );
            tracing::info!(
                dataset = %dataset_path.display(),
                exists = dataset_path.exists(),
                "dataset resolved"
            );
            tracing::info!(cache_dir = %thumbnail_root.display(), "thumbnail cache dir resolved");
            // DBオープン + migration (WP-1)。失敗時もアプリは続行する
            // (db_status と health.db_open で状態を観測できる)。
            let db_path = db::resolve_db_path(&args, &app_dirs);
            tracing::info!(db = %db_path.display(), "database path resolved");
            let db_state = db::DbState::open(db_path);
            if let Some(err) = &db_state.open_error {
                tracing::warn!(error = %err, "database unavailable; continuing without DB");
            }
            let events = Box::new(thumbnails::TauriThumbEvents {
                app: app.handle().clone(),
            });
            let service = match ThumbnailService::new(thumbnail_root, events) {
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
            state.db = Some(db_state);
            // WP-2: 登録ルート制御。DBが開けている場合のみ生成し、
            // 有効ルートの監視を開始して初回スキャンを非同期投入する
            // (UIブロックなし。再起動復元はスナップショット生成で担保)。
            if let Some(db) = state.db.as_ref().and_then(|d| d.db.clone()) {
                match library::LibraryController::new(
                    db.clone(),
                    std::sync::Arc::new(library::TauriLibraryHost {
                        app: app.handle().clone(),
                    }),
                ) {
                    Ok(controller) => {
                        match controller.list_roots() {
                            Ok(roots) => {
                                for (root, available) in roots {
                                    if root.enabled && root.watch_enabled && available {
                                        if let Err(err) = spawn_root_watcher(&controller, &root) {
                                            tracing::warn!(
                                                root_id = root.id,
                                                error = %err,
                                                "failed to start watcher"
                                            );
                                        }
                                    }
                                }
                            }
                            Err(err) => {
                                tracing::warn!(error = %err, "failed to list roots at startup")
                            }
                        }
                        // WP-7: ファイル操作サービス + 起動時回復
                        // (executing残留の確定と .gento-tmp- 掃除)。
                        let file_ops_service = fileops::FileOpsService::new(
                            db,
                            std::sync::Arc::new(fileops::TauriFileOpsHost {
                                app: app.handle().clone(),
                            }),
                        );
                        for notice in file_ops_service.recover_at_startup() {
                            tracing::warn!(notice = %notice, "file ops recovery");
                        }
                        state.file_ops = Some(file_ops_service);
                        controller.enqueue_scan_all();
                        state.library = Some(controller);
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "library controller unavailable")
                    }
                }
            }
            app.manage(state);
            if app.get_webview_window("main").is_some() {
                tracing::info!("main window created");
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build tauri application")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                if let Some(state) = app.try_state::<AppState>() {
                    if let Some(service) = &state.thumbnails {
                        tracing::info!("shutting down thumbnail service");
                        service.shutdown();
                    }
                    if let Some(controller) = &state.library {
                        tracing::info!("shutting down library controller");
                        controller.shutdown();
                    }
                }
            }
        });
}

/// `GENTO_LOG` (tracing EnvFilter書式、既定 `info`) でstdoutへログを出す。
///
/// 形式はtracing-subscriber fmtの既定: `TIMESTAMP LEVEL TARGET: message`
/// (例: `2026-08-30T… INFO gento_tauri_lib: dataset resolved`)。
/// 計測ドライバは `devtest:` (target) と `scan metrics` (メッセージ冒頭) を
/// grepして採取するため、この形式を変える場合はドライバ側の追従が必要。
fn init_tracing() {
    let filter = EnvFilter::try_from_env("GENTO_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stdout)
        .init();
}

/// アプリケーションディレクトリを解決する (WP-0)。
///
/// - data_dir: `dirs::data_dir()/gento-spike` — MVP-WP-1のSQLite DBはここへ置く
/// - cache_dir: `dirs::cache_dir()/gento-spike` — サムネイル等の再生成可能データ
/// - log_dir: `dirs::cache_dir()/gento-spike/log` — panic.log等の診断出力
///
/// 不正時 (HOME喪失等) は一時領域へフォールバックする。解決はこのTauri
/// アダプター側で行い、コア (`gento_core::AppDirs`) は値のみを受け取る。
fn resolve_app_dirs() -> AppDirs {
    let cache_base = dirs::cache_dir().unwrap_or_else(std::env::temp_dir);
    let data_base = dirs::data_dir().unwrap_or_else(std::env::temp_dir);
    AppDirs {
        data_dir: data_base.join(DIR_NAMESPACE),
        cache_dir: cache_base.join(DIR_NAMESPACE),
        log_dir: cache_base.join(DIR_NAMESPACE).join("log"),
    }
}

/// サムネイルキャッシュのルート。`--cache-dir <PATH>` 上書き (計測ドライバが
/// 使用) はサムネイルルートのみに効き、`AppDirs` の既定値は動かさない
/// (後方互換: 上書き時もlog/DB位置はユーザー既定のまま)。
fn resolve_thumbnail_root(args: &[String], app_dirs: &AppDirs) -> PathBuf {
    if let Some(v) = flag_value(args, "--cache-dir") {
        return PathBuf::from(v);
    }
    app_dirs.cache_dir.join("thumbnails")
}

/// `--dataset <PATH>` を解析する。指定がなければCWD相対の `sample/dataset`。
fn resolve_dataset_path(args: &[String]) -> PathBuf {
    let value = flag_value(args, "--dataset");
    match value {
        Some(v) => PathBuf::from(v),
        None => PathBuf::from("sample/dataset"),
    }
}

/// フラグ形式 `--name value` の値を取り出す。値なしの場合は既定へフォールバックしwarn。
/// db.rs の `--db` 解決からも使うため crate内公開。
pub(crate) fn flag_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
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

/// setup時の監視開始 (controllerの公開ラッパ)。
fn spawn_root_watcher(
    controller: &std::sync::Arc<library::LibraryController>,
    root: &gento_core::repo::LibraryRootRecord,
) -> Result<(), String> {
    controller.start_root_watcher(root)
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
    fn thumbnail_root_honors_cache_dir_flag() {
        let dirs = resolve_app_dirs();
        let args = vec!["--cache-dir".to_string(), "/tmp/mycache".to_string()];
        assert_eq!(
            resolve_thumbnail_root(&args, &dirs),
            PathBuf::from("/tmp/mycache")
        );
        // datasetとの共存
        let both = vec![
            "--dataset".to_string(),
            "/data".to_string(),
            "--cache-dir".to_string(),
            "/tmp/c2".to_string(),
        ];
        assert_eq!(resolve_dataset_path(&both), PathBuf::from("/data"));
        assert_eq!(
            resolve_thumbnail_root(&both, &dirs),
            PathBuf::from("/tmp/c2")
        );
    }

    #[test]
    fn default_thumbnail_root_is_under_user_cache() {
        let dirs = resolve_app_dirs();
        let base = dirs::cache_dir().unwrap_or_else(std::env::temp_dir);
        assert_eq!(
            resolve_thumbnail_root(&[], &dirs),
            base.join(DIR_NAMESPACE).join("thumbnails")
        );
    }

    // ===== アプリディレクトリ解決 (WP-0) =====

    #[test]
    fn app_dirs_follow_xdg_layout() {
        let dirs = resolve_app_dirs();
        let cache_base = dirs::cache_dir().unwrap_or_else(std::env::temp_dir);
        let data_base = dirs::data_dir().unwrap_or_else(std::env::temp_dir);
        assert_eq!(dirs.cache_dir, cache_base.join(DIR_NAMESPACE));
        assert_eq!(dirs.data_dir, data_base.join(DIR_NAMESPACE));
        assert_eq!(dirs.log_dir, cache_base.join(DIR_NAMESPACE).join("log"));
    }

    #[test]
    fn cache_dir_flag_does_not_move_app_dirs() {
        // --cache-dir はサムネイルルート専用の上書き (計測ドライバ互換)。
        // AppDirs (data/log) は常にユーザー既定位置のまま。
        let dirs = resolve_app_dirs();
        let args = vec![
            "--cache-dir".to_string(),
            "/tmp/measurement-cache".to_string(),
        ];
        assert_eq!(
            resolve_thumbnail_root(&args, &dirs),
            PathBuf::from("/tmp/measurement-cache")
        );
        assert_eq!(
            dirs.cache_dir,
            dirs::cache_dir().unwrap().join(DIR_NAMESPACE)
        );
    }

    // ===== panic hook (WP-0): 診断行の実ファイル出力 =====

    #[test]
    fn panic_hook_writes_diagnostic_line_to_log_dir() {
        let dir = std::env::temp_dir().join(format!("gento-panic-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let dirs = AppDirs {
            data_dir: dir.clone(),
            cache_dir: dir.clone(),
            log_dir: dir.join("log"),
        };
        let original = std::panic::take_hook();
        install_panic_hook(&dirs);
        let result = std::panic::catch_unwind(|| panic!("wp0 panic probe"));
        std::panic::set_hook(original);
        assert!(result.is_err(), "catch_unwind must capture the panic");

        let content = std::fs::read_to_string(dirs.log_dir.join("panic.log")).expect("panic.log");
        assert!(content.contains("wp0 panic probe"), "{content}");
        assert!(content.contains(env!("CARGO_PKG_VERSION")), "{content}");
        assert!(
            content.contains(&format!("api_version={GENTO_API_VERSION}")),
            "{content}"
        );
        assert!(content.contains("os="), "{content}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
