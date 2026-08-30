//! 計測ブリッジ (devtest) のcommand群 (WP-0でdevtools機能へ隔離)。
//!
//! このモジュール全体が `debug_assertions` または `--features devtools`
//! でのみコンパイルされる。通常releaseビルドにはinvoke handlerごと
//! 存在しない (artifact-level保証: scripts/release-smoke.sh)。
//!
//! devtoolsビルド内では更にランタイムゲートとして環境変数
//! `GENTO_DEV_BRIDGE` (値が厳密に `"1"` のときのみ有効) を使う。
//! 計測ドライバ (docs/dev/measurements/drivers/) との互換のため
//! この2段構えを維持する。

use tauri::State;

use crate::catalog::AppState;

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

/// 実機計測ブリッジ (devtest.ts) の有効化。devtoolsビルド内では
/// 環境変数 `GENTO_DEV_BRIDGE` が追加のランタイムゲート。
/// (pubはgenerate_handlerのマクロ展開に必要。catalog等のcommandと同じ)
#[tauri::command]
pub fn dev_bridge_enabled(state: State<'_, AppState>) -> Result<bool, String> {
    dev_bridge_enabled_impl(&state)
}

/// 計測ブリッジから機械可読な行をログへ流す (bash側は `devtest:` でgrepする)。
/// ブリッジ無効時はErr (書き込み不能)。
#[tauri::command]
pub fn devtest_report(state: State<'_, AppState>, payload: String) -> Result<(), String> {
    devtest_report_impl(&state, &payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::path::PathBuf;

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
        use crate::thumbnails::{ThumbEvents, ThumbFailedItem, ThumbReadyItem};

        struct NoEvents;
        impl ThumbEvents for NoEvents {
            fn ready(&self, _items: &[ThumbReadyItem]) {}
            fn failed(&self, _items: &[ThumbFailedItem]) {}
        }

        let cache_root =
            std::env::temp_dir().join(format!("gento-lib-gate-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cache_root);
        let service =
            crate::thumbnails::ThumbnailService::new(cache_root.clone(), Box::new(NoEvents))
                .expect("service");
        // テスト実行時の環境変数に依存しないようフラグを明示する
        let mut state = AppState::new(PathBuf::from("/tmp/gento-test-dataset"));
        state.thumbnails = Some(std::sync::Arc::clone(&service));
        state
            .dev_bridge
            .store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(dev_bridge_enabled_impl(&state).is_err());
        assert!(devtest_report_impl(&state, "G line").is_err());
        assert!(
            crate::thumbnails::set_failure_injection_impl(&state, "decode", 0.5).is_err(),
            "set_failure_injection must be gated"
        );

        state
            .dev_bridge
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(dev_bridge_enabled_impl(&state), Ok(true));
        assert!(devtest_report_impl(&state, "G line").is_ok());
        let dto = crate::thumbnails::set_failure_injection_impl(&state, "none", 0.0).unwrap();
        assert_eq!(dto.mode, "none");
        assert_eq!(dto.rate, 0.0);
        // 無効時と異なり、不正な引数は通常どおり引数検証で拒否される
        assert!(crate::thumbnails::set_failure_injection_impl(&state, "bogus", 0.5).is_err());
        assert!(crate::thumbnails::set_failure_injection_impl(&state, "decode", 1.5).is_err());
        service.shutdown();
        let _ = std::fs::remove_dir_all(&cache_root);
    }
}
