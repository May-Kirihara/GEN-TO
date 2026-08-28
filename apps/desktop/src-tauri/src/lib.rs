//! GEN-TO 技術検証プロトタイプのデスクトップシェル。
//!
//! 本crateはTauri commandとプロトコル登録の薄いアダプターに限定する。
//! ビジネスロジックはgento-core / gento-imageへ置く
//! (docs/implementation-plan.md §5, §5.1)。

pub mod catalog;
pub mod thumbnails;

use std::path::{Path, PathBuf};

use catalog::AppState;
use gento_core::HealthInfo;
use tauri::{Manager, RunEvent, State};
use tracing_subscriber::EnvFilter;

use crate::thumbnails::ThumbnailService;

const APP_NAME: &str = "GEN-TO 技術検証プロトタイプ";

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

pub fn run() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    tauri::Builder::default()
        .setup(move |app| {
            init_tracing();
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
            thumbnails::clear_thumbnail_cache
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
}
