//! GEN-TO 技術検証プロトタイプのデスクトップシェル。
//!
//! 本crateはTauri commandとプロトコル登録の薄いアダプターに限定する。
//! ビジネスロジックはgento-core / gento-imageへ置く
//! (docs/implementation-plan.md §5, §5.1)。

pub mod catalog;

use std::path::{Path, PathBuf};

use catalog::AppState;
use gento_core::HealthInfo;
use tauri::{Manager, State};
use tracing_subscriber::EnvFilter;

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
            tracing::info!(
                dataset = %dataset_path.display(),
                exists = dataset_path.exists(),
                "dataset resolved"
            );
            app.manage(AppState::new(dataset_path));
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
            catalog::set_catalog_mode
        ])
        .run(tauri::generate_context!())
        .expect("failed to run tauri application");
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
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--dataset" {
            match iter.next() {
                Some(value) => return PathBuf::from(value),
                None => tracing::warn!("--dataset without a value; using default path"),
            }
        }
    }
    PathBuf::from("sample/dataset")
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
}
