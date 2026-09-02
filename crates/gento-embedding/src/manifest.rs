//! GEN-TO ruri-v3 ONNXモデルパッケージのmanifest (MVP WP-11)。
//!
//! 正本は `docs/dev/ruri3-onnx-manifest.json` (CONV-1成果物) であり、
//! このcrateはそれを [`include_str!`] で埋め込む — 正本から生成された
//! コピーを別管理すると乖離しうるため、**正本ファイルを直接埋め込む**
//! 方式を採る (ビルド時にdocs/dev配下を参照する。crate単独publishは
//! しないworkspace内運用)。
//!
//! 検証方式: パッケージディレクトリ内の各ファイルについて
//! sha256 + サイズをmanifest値と比較する (embedding-runtime.md §3)。

use serde::Deserialize;
use sha2::{Digest, Sha256};

/// 埋め込みmanifest正本 (docs/dev/ruri3-onnx-manifest.json)。
const MANIFEST_JSON: &str = include_str!("../../../docs/dev/ruri3-onnx-manifest.json");

/// manifest全体 (正本の型。変換環境等の未使用フィールドも保持)。
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub packages: std::collections::BTreeMap<String, ManifestPackage>,
}

/// 1モデルパッケージのmanifestエントリ。
#[derive(Debug, Clone, Deserialize)]
pub struct ManifestPackage {
    pub package_name: String,
    pub package_version: u32,
    pub source: ManifestSource,
    pub model: ManifestModel,
    pub files: std::collections::BTreeMap<String, ManifestFile>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ManifestSource {
    pub repository_id: String,
    pub revision: String,
    pub license: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ManifestModel {
    pub embedding_dim: u32,
    pub max_input_length: u32,
    /// "cosine" 以外は現状非対応 (検証で拒否)。
    pub distance_metric: String,
    /// "mean_tokens" 以外は非対応。
    pub pooling: String,
    /// "l2" 以外は非対応。
    pub normalization: String,
    pub query_prefix: String,
    pub document_prefix: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ManifestFile {
    pub sha256: String,
    pub size_bytes: u64,
}

/// 埋め込みmanifestをパースした結果。
pub fn embedded_manifest() -> Result<Manifest, String> {
    let manifest: Manifest = serde_json::from_str(MANIFEST_JSON)
        .map_err(|e| format!("埋め込みmanifest解析失敗: {e}"))?;
    Ok(manifest)
}

/// 標準モデルID (IPC文字列・models_dir直下のディレクトリ名)。
pub const MODEL_RURI_V3_70M: &str = "ruri-v3-70m";
/// 軽量モデルID。
pub const MODEL_RURI_V3_30M: &str = "ruri-v3-30m";

/// 選択可能な全モデルID (既定=70m順)。
pub const MODEL_IDS: [&str; 2] = [MODEL_RURI_V3_70M, MODEL_RURI_V3_30M];

/// パッケージの検証結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageVerification {
    /// 全ファイルのsha256・サイズがmanifestと一致。
    Verified,
    /// ディレクトリが存在しない (未ダウンロード・未配置)。
    Missing,
    /// 一部ファイルが欠落 (検証不能)。
    MissingFiles(Vec<String>),
    /// sha256またはサイズ不一致 (破損・改変)。
    Mismatch { file: String, reason: String },
}

impl PackageVerification {
    pub fn is_verified(&self) -> bool {
        matches!(self, PackageVerification::Verified)
    }

    /// UI表示用の日本語説明。
    pub fn describe(&self) -> String {
        match self {
            PackageVerification::Verified => "検証済み".to_string(),
            PackageVerification::Missing => "未インストール".to_string(),
            PackageVerification::MissingFiles(files) => {
                format!("ファイル欠落: {}", files.join(", "))
            }
            PackageVerification::Mismatch { file, reason } => {
                format!("検証失敗 ({file}): {reason}")
            }
        }
    }
}

/// モデル1種の実行仕様 (manifest由来。provider・ワーカーが使用)。
#[derive(Debug, Clone)]
pub struct ModelSpec {
    pub model_id: String,
    pub package_name: String,
    pub package_version: u32,
    pub repository_id: String,
    pub revision: String,
    pub license: String,
    pub embedding_dim: usize,
    pub max_input_length: usize,
    pub query_prefix: String,
    pub document_prefix: String,
    /// 検証対象ファイル名と期待ハッシュ・サイズ。
    pub files: std::collections::BTreeMap<String, ManifestFile>,
}

/// manifestからモデル実行仕様を取り出す。未知モデルID・非対応設定
/// (距離関数等) はErr。
pub fn model_spec(model_id: &str) -> Result<ModelSpec, String> {
    let manifest = embedded_manifest()?;
    let package = manifest
        .packages
        .get(model_id)
        .ok_or_else(|| format!("不明なモデルIDです: {model_id}"))?;
    if package.model.distance_metric != "cosine" {
        return Err(format!(
            "非対応の距離関数です: {} (cosineのみ対応)",
            package.model.distance_metric
        ));
    }
    if package.model.pooling != "mean_tokens" {
        return Err(format!(
            "非対応のpoolingです: {} (mean_tokensのみ対応)",
            package.model.pooling
        ));
    }
    if package.model.normalization != "l2" {
        return Err(format!(
            "非対応の正規化です: {} (l2のみ対応)",
            package.model.normalization
        ));
    }
    Ok(ModelSpec {
        model_id: model_id.to_string(),
        package_name: package.package_name.clone(),
        package_version: package.package_version,
        repository_id: package.source.repository_id.clone(),
        revision: package.source.revision.clone(),
        license: package.source.license.clone(),
        embedding_dim: package.model.embedding_dim as usize,
        max_input_length: package.model.max_input_length as usize,
        query_prefix: package.model.query_prefix.clone(),
        document_prefix: package.model.document_prefix.clone(),
        files: package.files.clone(),
    })
}

/// ファイル1つのsha256を計算 (ストリーミング。モデル280MB対応)。
fn sha256_of_file(path: &std::path::Path) -> Result<String, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("open失敗: {e}"))?;
    let mut reader = std::io::BufReader::with_capacity(1024 * 1024, file);
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 1024 * 512];
    loop {
        let read =
            std::io::Read::read(&mut reader, &mut buffer).map_err(|e| format!("read失敗: {e}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    Ok(hex_lower(&digest))
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// モデルパッケージディレクトリ (`models_dir/<model_id>`) をmanifestで
/// 検証する。アプリはこの検証をパスしたパッケージのみ有効化する
/// (embedding-runtime.md §4「検証成功後のアトミックな有効化」)。
pub fn verify_package(models_dir: &std::path::Path, model_id: &str) -> PackageVerification {
    let spec = match model_spec(model_id) {
        Ok(spec) => spec,
        Err(e) => {
            return PackageVerification::Mismatch {
                file: "<manifest>".to_string(),
                reason: e,
            }
        }
    };
    verify_package_with_spec(&spec, &models_dir.join(model_id))
}

/// specのfilesエントリでディレクトリを検証する (テストで自家製specを
/// 与えられるように分離)。
pub fn verify_package_with_spec(
    spec: &ModelSpec,
    package_dir: &std::path::Path,
) -> PackageVerification {
    verify_files(package_dir, &spec.files)
}

/// ファイル群のエントリ (名前 → sha256・サイズ) でディレクトリを検証する。
/// download.rs等がmanifest以外のファイル集合で再利用するため公開。
pub fn verify_files(
    package_dir: &std::path::Path,
    files: &std::collections::BTreeMap<String, ManifestFile>,
) -> PackageVerification {
    if !package_dir.is_dir() {
        return PackageVerification::Missing;
    }
    let mut missing = Vec::new();
    for name in files.keys() {
        if !package_dir.join(name).is_file() {
            missing.push(name.clone());
        }
    }
    if !missing.is_empty() {
        return PackageVerification::MissingFiles(missing);
    }
    for (name, expected) in files {
        let path = package_dir.join(name);
        let size = match std::fs::metadata(&path) {
            Ok(meta) => meta.len(),
            Err(e) => {
                return PackageVerification::Mismatch {
                    file: name.clone(),
                    reason: format!("metadata失敗: {e}"),
                }
            }
        };
        if size != expected.size_bytes {
            return PackageVerification::Mismatch {
                file: name.clone(),
                reason: format!(
                    "サイズ不一致: 実測 {size} bytes ≠ 期待 {} bytes",
                    expected.size_bytes
                ),
            };
        }
        match sha256_of_file(&path) {
            Ok(actual) => {
                if !actual.eq_ignore_ascii_case(&expected.sha256) {
                    return PackageVerification::Mismatch {
                        file: name.clone(),
                        reason: format!("sha256不一致: 実測 {actual}"),
                    };
                }
            }
            Err(e) => {
                return PackageVerification::Mismatch {
                    file: name.clone(),
                    reason: e,
                }
            }
        }
    }
    PackageVerification::Verified
}

#[cfg(test)]
mod tests {
    use super::*;

    /// tempfileクレート不使用の規約: pid + nanosの一時ディレクトリ。
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "gento-embed-manifest-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).expect("temp dir");
            Self(dir)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn embedded_manifest_parses_and_has_both_models() {
        let manifest = embedded_manifest().expect("manifest");
        assert!(manifest.packages.contains_key(MODEL_RURI_V3_70M));
        assert!(manifest.packages.contains_key(MODEL_RURI_V3_30M));
    }

    #[test]
    fn model_specs_match_conversion_contract() {
        // CONV-1検証済みの定数 (docs/dev/ruri3-onnx-manifest.json)。
        let m70 = model_spec(MODEL_RURI_V3_70M).expect("70m");
        assert_eq!(m70.embedding_dim, 384);
        assert_eq!(m70.max_input_length, 8192);
        assert_eq!(m70.query_prefix, "検索クエリ: ");
        assert_eq!(m70.document_prefix, "検索文書: ");
        assert_eq!(m70.repository_id, "cl-nagoya/ruri-v3-70m");
        assert_eq!(m70.revision, "07a8b0aba47d29d2ca21f89b915c1efe2c23d1cc");
        assert_eq!(m70.license, "Apache-2.0");
        assert_eq!(m70.model_id, MODEL_RURI_V3_70M);
        // 検証対象ファイル (model.onnx・tokenizer.json等)
        for required in ["model.onnx", "tokenizer.json", "LICENSE", "NOTICE"] {
            assert!(m70.files.contains_key(required), "missing {required}");
        }

        let m30 = model_spec(MODEL_RURI_V3_30M).expect("30m");
        assert_eq!(m30.embedding_dim, 256);
        // tokenizer.jsonは両モデルで同一 (CONV-1確認済み)
        assert_eq!(
            m70.files.get("tokenizer.json").unwrap().sha256,
            m30.files.get("tokenizer.json").unwrap().sha256
        );
    }

    #[test]
    fn unknown_model_id_is_rejected() {
        assert!(model_spec("ruri-v3-130m").is_err());
        assert!(model_spec("").is_err());
    }

    #[test]
    fn verify_package_reports_missing_dir() {
        let dir = TempDir::new("missing");
        assert_eq!(
            verify_package(dir.path(), MODEL_RURI_V3_70M),
            PackageVerification::Missing
        );
        assert_eq!(
            verify_package(dir.path(), MODEL_RURI_V3_70M).describe(),
            "未インストール"
        );
    }

    #[test]
    fn verify_package_detects_missing_files() {
        let dir = TempDir::new("partial");
        let package = dir.path().join(MODEL_RURI_V3_30M);
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("LICENSE"), b"x").unwrap();
        let result = verify_package(dir.path(), MODEL_RURI_V3_30M);
        match &result {
            PackageVerification::MissingFiles(files) => {
                assert!(!files.is_empty());
                assert!(files.contains(&"model.onnx".to_string()));
            }
            other => panic!("期待と違う検証結果: {other:?}"),
        }
        assert!(result.describe().contains("ファイル欠落"));
    }

    #[test]
    fn verify_package_detects_size_and_hash_mismatch() {
        let dir = TempDir::new("corrupt");
        let package = dir.path().join(MODEL_RURI_V3_30M);
        std::fs::create_dir_all(&package).unwrap();
        // NOTICEのみを含む自家製specでサイズ・hash不一致を検出する
        let notice_spec = {
            let notice = model_spec(MODEL_RURI_V3_30M)
                .unwrap()
                .files
                .get("NOTICE")
                .unwrap()
                .clone();
            ModelSpec {
                model_id: MODEL_RURI_V3_30M.to_string(),
                package_name: "test".to_string(),
                package_version: 1,
                repository_id: "t".to_string(),
                revision: "t".to_string(),
                license: "t".to_string(),
                embedding_dim: 4,
                max_input_length: 8,
                query_prefix: "q".to_string(),
                document_prefix: "d".to_string(),
                files: std::collections::BTreeMap::from([("NOTICE".to_string(), notice)]),
            }
        };
        // 正しいサイズで不正内容 (size checkを通してhashで落とす)
        let bogus = vec![0u8; notice_spec.files["NOTICE"].size_bytes as usize];
        std::fs::write(package.join("NOTICE"), &bogus).unwrap();
        match verify_package_with_spec(&notice_spec, &package) {
            PackageVerification::Mismatch { file, reason } => {
                assert_eq!(file, "NOTICE");
                assert!(
                    reason.contains("sha256不一致") || reason.contains("サイズ不一致"),
                    "{reason}"
                );
            }
            other => panic!("期待と違う検証結果: {other:?}"),
        }
        // サイズ不一致単独のケース
        std::fs::write(package.join("NOTICE"), b"short").unwrap();
        match verify_package_with_spec(&notice_spec, &package) {
            PackageVerification::Mismatch { reason, .. } => {
                assert!(reason.contains("サイズ不一致"), "{reason}");
            }
            other => panic!("サイズ不一致を検出すべき: {other:?}"),
        }
    }

    #[test]
    fn verify_package_passes_for_matching_files() {
        // 自家製spec (ハッシュを実内容から計算) でVerified経路を駆動する。
        let dir = TempDir::new("ok");
        let package = dir.path().join(MODEL_RURI_V3_30M);
        std::fs::create_dir_all(&package).unwrap();
        let notice_content = b"gento test notice";
        let model_content = vec![7u8; 4096];
        std::fs::write(package.join("NOTICE"), notice_content).unwrap();
        std::fs::write(package.join("model.onnx"), &model_content).unwrap();
        let mut hasher = Sha256::new();
        hasher.update(notice_content);
        let notice_hash = hex_lower(&hasher.finalize());
        let mut hasher = Sha256::new();
        hasher.update(&model_content);
        let model_hash = hex_lower(&hasher.finalize());
        let spec = ModelSpec {
            model_id: MODEL_RURI_V3_30M.to_string(),
            package_name: "test".to_string(),
            package_version: 1,
            repository_id: "test/repo".to_string(),
            revision: "deadbeef".to_string(),
            license: "Apache-2.0".to_string(),
            embedding_dim: 4,
            max_input_length: 128,
            query_prefix: "検索クエリ: ".to_string(),
            document_prefix: "検索文書: ".to_string(),
            files: std::collections::BTreeMap::from([
                (
                    "NOTICE".to_string(),
                    ManifestFile {
                        sha256: notice_hash,
                        size_bytes: notice_content.len() as u64,
                    },
                ),
                (
                    "model.onnx".to_string(),
                    ManifestFile {
                        sha256: model_hash,
                        size_bytes: model_content.len() as u64,
                    },
                ),
            ]),
        };
        assert_eq!(
            verify_package_with_spec(&spec, &package),
            PackageVerification::Verified
        );
        assert!(verify_package_with_spec(&spec, &package).is_verified());
        // 大文字ハッシュ (manifestは小文字前提だがeq_ignore_ascii_caseで許容)
        let mut upper = spec.clone();
        upper.files.get_mut("NOTICE").unwrap().sha256 = upper.files["NOTICE"].sha256.to_uppercase();
        assert!(verify_package_with_spec(&upper, &package).is_verified());
    }
}
