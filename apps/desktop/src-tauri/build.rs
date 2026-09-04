fn main() {
    // WP-18 E2: バージョン・ビルド情報 (About表示用) の埋め込み。
    // - commit hash: gitが使えない環境 (tarball展開等) では "unknown" に
    //   フォールバックし、ビルドを失敗させない。
    // - ビルド日時は埋め込まない (再現可能ビルドを優先)。
    let commit = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| s.len() >= 7)
        .unwrap_or_else(|| "unknown".to_string());
    let short = commit[..7].to_string();
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=GENTO_BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=GENTO_BUILD_COMMIT_SHORT={short}");
    println!("cargo:rustc-env=GENTO_BUILD_PROFILE={profile}");
    // コミット直後にビルドし直した際のhash更新取りこぼしを避けるため、
    // git参照の変化でbuild.rsを再実行する (コード変更による再実行は
    // 通常の再コンパイル経路で発生する)。
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs");
    tauri_build::build()
}
