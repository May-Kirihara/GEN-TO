//! モデル管理 (配置場所解決と検証。docs/embedding-runtime.md §3/§4)。
//!
//! 取得順序 (WP-11設計 — ローカルdir第一):
//! 1. `--models-dir <path>` 引数 (アプリ・E2Eドライバ用)
//! 2. `GENTO_MODELS_DIR` 環境変数
//! 3. `AppDirs.data_dir/models` (既定)
//!
//! HFダウンロード (download.rs) は「配布repo URLが設定された場合のみ」
//! 有効。MVP既定は未設定 = ローカル運用。

use std::path::{Path, PathBuf};

use crate::manifest::{verify_package, PackageVerification};

/// モデルディレクトリ解決の優先入力。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelsDirOptions {
    /// `--models-dir` 引数の値。
    pub flag: Option<PathBuf>,
    /// `GENTO_MODELS_DIR` 環境変数の値 (呼び出し側が読み取って渡す。
    /// テストで差し替えられるように注入形式)。
    pub env: Option<PathBuf>,
    /// アプリデータディレクトリ (既定の基底)。
    pub data_dir: Option<PathBuf>,
}

/// モデルルートディレクトリを解決する。
pub fn resolve_models_dir(options: &ModelsDirOptions) -> PathBuf {
    if let Some(flag) = &options.flag {
        return flag.clone();
    }
    if let Some(env) = &options.env {
        return env.clone();
    }
    match &options.data_dir {
        Some(data_dir) => data_dir.join("models"),
        None => PathBuf::from("models"),
    }
}

/// 既定の解決 (引数・環境変数を実際に読む。アプリのsetupで使用)。
pub fn resolve_models_dir_from_env(args: &[String], data_dir: &Path) -> PathBuf {
    let flag = args
        .iter()
        .position(|a| a == "--models-dir")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);
    let env = std::env::var("GENTO_MODELS_DIR").ok().map(PathBuf::from);
    resolve_models_dir(&ModelsDirOptions {
        flag,
        env,
        data_dir: Some(data_dir.to_path_buf()),
    })
}

/// 全モデルの検証状態スナップショット (`embedding_status` 等で使用)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelPackageState {
    pub model_id: String,
    pub verification: PackageVerification,
    /// パッケージディレクトリ (UIの保存先表示)。
    pub package_dir: PathBuf,
}

/// models_dir配下の全既知モデルの状態を収集する。
/// 検証失敗・不在でもエラーにしない (アプリは起動継続 — acceptance 5-1)。
pub fn scan_models(models_dir: &Path) -> Vec<ModelPackageState> {
    crate::manifest::MODEL_IDS
        .iter()
        .map(|model_id| ModelPackageState {
            model_id: model_id.to_string(),
            verification: verify_package(models_dir, model_id),
            package_dir: models_dir.join(model_id),
        })
        .collect()
}

/// 検証済みパッケージのディレクトリを返す (未検証 = None)。
pub fn verified_package_dir(models_dir: &Path, model_id: &str) -> Option<PathBuf> {
    if verify_package(models_dir, model_id).is_verified() {
        Some(models_dir.join(model_id))
    } else {
        None
    }
}

/// モデルパッケージを削除する (`embedding_model_delete`)。
/// 検証状態を問わずディレクトリごと削除する。存在しない場合は
/// 何もしない (冪等)。
pub fn delete_package(models_dir: &Path, model_id: &str) -> Result<bool, String> {
    if !crate::provider::is_known_model(model_id) {
        return Err(format!("不明なモデルIDです: {model_id}"));
    }
    let package_dir = models_dir.join(model_id);
    if !package_dir.is_dir() {
        return Ok(false);
    }
    std::fs::remove_dir_all(&package_dir)
        .map_err(|e| format!("{} の削除失敗: {e}", package_dir.display()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_order_flag_env_default() {
        let options = ModelsDirOptions {
            flag: Some(PathBuf::from("/flag")),
            env: Some(PathBuf::from("/env")),
            data_dir: Some(PathBuf::from("/data")),
        };
        assert_eq!(resolve_models_dir(&options), PathBuf::from("/flag"));

        let options = ModelsDirOptions {
            flag: None,
            env: Some(PathBuf::from("/env")),
            data_dir: Some(PathBuf::from("/data")),
        };
        assert_eq!(resolve_models_dir(&options), PathBuf::from("/env"));

        let options = ModelsDirOptions {
            flag: None,
            env: None,
            data_dir: Some(PathBuf::from("/data")),
        };
        assert_eq!(resolve_models_dir(&options), PathBuf::from("/data/models"));

        // すべて無い場合は相対パス (実運用ではdata_dirが必ず入る)
        let options = ModelsDirOptions::default();
        assert_eq!(resolve_models_dir(&options), PathBuf::from("models"));
    }

    #[test]
    fn scan_models_reports_missing_without_error() {
        let dir = std::env::temp_dir().join(format!(
            "gento-modelstore-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let states = scan_models(&dir);
        assert_eq!(states.len(), 2);
        assert!(states
            .iter()
            .all(|s| s.verification == PackageVerification::Missing));
        assert!(states[0].package_dir.ends_with("ruri-v3-70m"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verified_package_dir_none_for_missing() {
        let dir = std::env::temp_dir().join("gento-modelstore-none");
        assert!(verified_package_dir(&dir, "ruri-v3-70m").is_none());
    }

    #[test]
    fn delete_package_rejects_unknown_and_is_idempotent() {
        let dir = std::env::temp_dir().join(format!(
            "gento-modelstore-del-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::create_dir_all(dir.join("ruri-v3-30m"));
        assert!(delete_package(&dir, "bogus-model").is_err());
        assert!(delete_package(&dir, "ruri-v3-30m").unwrap(), "削除実行");
        assert!(
            !delete_package(&dir, "ruri-v3-30m").unwrap(),
            "2回目はno-op"
        );
        assert!(!dir.join("ruri-v3-30m").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
