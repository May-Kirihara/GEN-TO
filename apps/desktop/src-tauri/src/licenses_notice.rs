//! THIRD-PARTY-NOTICES完全性の回帰検査 (FG-WP18-1)。
//!
//! 配布scope policy: Cargo.lockに存在する全package (workspaceメンバーと
//! build-tool系を含む) を `docs/dev/licenses.md` (= bundle内
//! `licenses/THIRD-PARTY-NOTICES.md`) に掲載する。`cargo metadata`の
//! 解決graphはfeature/target構成で変動し、optional依存
//! (cxx → cxxbridge-cmd → clap一族) が零れ得るため、検証の正本は
//! lockfile走査のみ。

/// リポジトリ直下 (CARGO_MANIFEST_DIR = apps/desktop/src-tauri の3階層上)。
fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

/// Cargo.lockの全 `[[package]]` を (name, version) の集合として返す。
/// source行の有無 (workspaceメンバーか否か) は区別しない — どちらも
/// licenses.mdでは `| name | version |` 行として掲載されるため。
fn cargo_lock_packages() -> std::collections::BTreeSet<(String, String)> {
    let text = std::fs::read_to_string(repo_root().join("Cargo.lock"))
        .expect("repo rootのCargo.lockが読めること");
    let mut packages = std::collections::BTreeSet::new();
    let mut name: Option<String> = None;
    let mut version: Option<String> = None;
    // 末尾に空行を補い、ファイル終端で未flushのstanzaも取り込む
    // (lockfileのversion = 3ヘッダはnameが無いままflushされ除外される)。
    for line in text.lines().chain(std::iter::once("")) {
        let line = line.trim_end();
        if line.is_empty() {
            if let (Some(n), Some(v)) = (name.take(), version.take()) {
                packages.insert((n, v));
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("name = ") {
            name = Some(rest.trim_matches('"').to_string());
        } else if let Some(rest) = line.strip_prefix("version = ") {
            version = Some(rest.trim_matches('"').to_string());
        }
    }
    packages
}

/// licenses.mdのRust表の行 (「## TypeScript」以降のnpm表は照合外)。
fn rust_notice_lines() -> Vec<String> {
    let text = std::fs::read_to_string(repo_root().join("docs/dev/licenses.md"))
        .expect("docs/dev/licenses.mdが読めること (scripts/gen-licenses.shで生成)");
    let mut in_rust = false;
    let mut lines = Vec::new();
    for line in text.lines() {
        if line.starts_with("## ") {
            in_rust = line.starts_with("## Rust");
            continue;
        }
        if in_rust && line.starts_with("| ") {
            lines.push(line.to_string());
        }
    }
    lines
}

#[test]
fn every_cargo_lock_package_is_listed_in_third_party_notices() {
    let notice_lines = rust_notice_lines();
    let missing: Vec<(String, String)> = cargo_lock_packages()
        .into_iter()
        .filter(|(name, version)| {
            let prefix = format!("| {name} | {version} |");
            !notice_lines.iter().any(|l| l.starts_with(&prefix))
        })
        .collect();
    assert!(
        missing.is_empty(),
        "Cargo.lockに存在するがlicenses.md未掲載: {missing:?} — \
         scripts/gen-licenses.shを再生成してコミットすること \
         (配布scope = Cargo.lock全package)"
    );
}

#[test]
fn notices_list_build_tool_packages_outside_cargo_metadata_graph() {
    // cxxのoptional依存chain (cxxbridge-cmd → clap → clap_builder →
    // anstyle/clap_lex) はcargo metadataの解決graphに現れないが
    // Cargo.lockには存在する (FG-WP18-1 rejectの対象5件)。名前で固定し、
    // generatorがmetadata基準に後退したら即座に検出する。
    let notice_lines = rust_notice_lines();
    for name in [
        "anstyle",
        "clap",
        "clap_builder",
        "clap_lex",
        "cxxbridge-cmd",
    ] {
        let prefix = format!("| {name} |");
        assert!(
            notice_lines.iter().any(|l| l.starts_with(&prefix)),
            "build-tool系クレート {name} がlicenses.mdのRust表に無い — \
             gen-licenses.shがCargo.lock走査を維持しているか確認すること"
        );
    }
}
