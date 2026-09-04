//! アプリのバージョン・ビルド情報 (WP-18 E2)。
//!
//! commit hash・profile は build.rs で埋め込んだ `GENTO_BUILD_*` 環境変数
//! (コンパイル時固定) を用いる。devtools feature (計測・検証専用) を含む
//! ビルドであるかも示す — 開発パネルの有無と区別する材料。

use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AboutInfoDto {
    pub version: String,
    pub api_version: String,
    pub commit: String,
    pub commit_short: String,
    /// "debug" | "release" (build.rsのPROFILE)。
    pub profile: String,
    /// 計測・検証専用機能 (devtools feature) を含むビルドか。
    pub devtools_build: bool,
}

/// ビルド情報の正規化: build.rsが未実行の経路 (あり得ないはずだが
/// 環境によっては空文字列になり得る) では "unknown" へ寄せる。
fn env_or_unknown(value: &'static str) -> String {
    if value.is_empty() {
        "unknown".to_string()
    } else {
        value.to_string()
    }
}

#[tauri::command]
pub fn about_info() -> AboutInfoDto {
    AboutInfoDto {
        version: env!("CARGO_PKG_VERSION").to_string(),
        api_version: gento_core::GENTO_API_VERSION.to_string(),
        // option_env!: rust-analyzer等、build.rsを経由しない解析では
        // 環境変数が未設定でもコンパイルエラーにしない。
        commit: env_or_unknown(option_env!("GENTO_BUILD_COMMIT").unwrap_or("")),
        commit_short: env_or_unknown(option_env!("GENTO_BUILD_COMMIT_SHORT").unwrap_or("")),
        profile: env_or_unknown(option_env!("GENTO_BUILD_PROFILE").unwrap_or("")),
        devtools_build: cfg!(feature = "devtools"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn about_info_contains_build_info() {
        let dto = about_info();
        assert!(!dto.version.is_empty());
        assert!(!dto.api_version.is_empty());
        assert!(!dto.commit.is_empty());
        assert!(!dto.commit_short.is_empty());
        assert!(!dto.profile.is_empty());
        // dev buildではdebug_assertions相当のprofileになる
        assert!(dto.profile == "debug" || dto.profile == "release" || dto.profile == "unknown");
    }

    #[test]
    fn env_or_unknown_replaces_empty() {
        assert_eq!(env_or_unknown(""), "unknown");
        assert_eq!(env_or_unknown("abc"), "abc");
    }
}
