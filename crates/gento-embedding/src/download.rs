//! モデルパッケージのダウンロード (docs/embedding-runtime.md §4)。
//!
//! 配布repo URL (`repo_base`) が設定されている場合のみ使用する
//! (MVP既定は未設定 = ローカル models_dir 運用)。要件:
//! - 進捗イベント (コールバック)
//! - キャンセル (AtomicBool)
//! - 中断再開 (HTTP Range。サーバー非対応時は最初からやり直し)
//! - 空き容量の事前確認 (statvfs)
//! - 一時ファイル (.part) → sha256検証 → アトミックrename
//! - プロキシ環境変数を尊重 (gento-aiのクライアントとは分離)
//!
//! 失敗・検証失敗時は既存モデルを維持し、不完全なパッケージを
//! 有効化しない (stagingディレクトリに置くため、最終renameまで
//! models_dir/<model> は存在しない)。

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};

/// DL進捗 (コールバック引数)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadProgress {
    pub file: String,
    pub file_bytes_done: u64,
    pub file_bytes_total: u64,
    /// パッケージ全体の進捗 (検証済みファイルを含む)。
    pub total_bytes_done: u64,
    pub total_bytes: u64,
}

/// DL結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadOutcome {
    /// DLして検証し有効化した。
    Downloaded { bytes: u64 },
    /// 既に検証済みパッケージが存在する (DL不要)。
    AlreadyPresent,
}

/// DLエラー。
#[derive(Debug)]
pub enum DownloadError {
    Cancelled,
    Io(String),
    Http(String),
    DiskSpace { needed: u64, available: u64 },
    Verification { file: String, reason: String },
    InvalidRepo(String),
}

impl std::fmt::Display for DownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DownloadError::Cancelled => write!(f, "ダウンロードをキャンセルしました"),
            DownloadError::Io(m) => write!(f, "IOエラー: {m}"),
            DownloadError::Http(m) => write!(f, "HTTPエラー: {m}"),
            DownloadError::DiskSpace { needed, available } => write!(
                f,
                "空き容量不足: 必要 {needed} bytes / 空き {available} bytes"
            ),
            DownloadError::Verification { file, reason } => {
                write!(f, "検証失敗 ({file}): {reason}")
            }
            DownloadError::InvalidRepo(m) => write!(f, "配布repo URLが不正です: {m}"),
        }
    }
}

impl std::error::Error for DownloadError {}

/// DL実行のオプション。
pub struct DownloadOptions<'a> {
    /// 配布repoのURL前置 (`{base}/{filename}` で各ファイルを取得する)。
    pub repo_base: &'a str,
    /// モデルID (パッケージのディレクトリ名)。
    pub model_id: &'a str,
    /// モデルルートディレクトリ (配置先)。
    pub models_dir: &'a Path,
    /// manifest由来の期待ファイル (名前 → sha256・サイズ)。
    pub files: &'a std::collections::BTreeMap<String, crate::manifest::ManifestFile>,
    /// 進捗コールバック (チャンク毎)。
    pub progress: &'a dyn Fn(&DownloadProgress),
    /// キャンセルフラグ (trueで観測された時点で中断)。
    pub cancel: &'a AtomicBool,
}

/// gento-aiと分離したHTTPクライアント。プロキシ環境変数
/// (https_proxy/HTTPS_PROXY/http_proxy/HTTP_PROXY) を尊重する
/// (workspaceのreqwestはsystem-proxy feature無効のため明示構成)。
fn build_client() -> Result<reqwest::blocking::Client, DownloadError> {
    let mut builder = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(15));
    if let Some(proxy) = proxy_from_env() {
        builder = builder.proxy(proxy);
    }
    builder
        .build()
        .map_err(|e| DownloadError::Http(format!("クライアント構築失敗: {e}")))
}

/// 環境変数からプロキシを構成する (no_proxyはreqwest側のProxyで扱う)。
fn proxy_from_env() -> Option<reqwest::Proxy> {
    for key in ["https_proxy", "HTTPS_PROXY", "http_proxy", "HTTP_PROXY"] {
        if let Ok(value) = std::env::var(key) {
            if value.is_empty() {
                continue;
            }
            if let Ok(proxy) = reqwest::Proxy::all(&value) {
                if let Ok(no_proxy) =
                    std::env::var("no_proxy").or_else(|_| std::env::var("NO_PROXY"))
                {
                    let filtered = reqwest::NoProxy::from_string(&no_proxy);
                    return Some(proxy.no_proxy(filtered));
                }
                return Some(proxy);
            }
        }
    }
    None
}

/// ディレクトリの空き容量 (bytes)。unix: statvfs (gento-platform集約実装)。
/// その他のOS・失敗時は None (確認skip — fail-open。書き込み失敗時に
/// 通常のIOエラーで落ちる)。
/// WP-17-1: linux限定の自前実装からgento-platformへ集約 — macOSでも
/// 空き容量確認が有効になった (旧実装はlinuxのみでmacOSはskip扱い)。
pub fn available_bytes(path: &Path) -> Option<u64> {
    gento_platform::dir_free_bytes(path)
}

/// 既定のユーザーエージェント。
const USER_AGENT: &str = concat!("gento-embedding/", env!("CARGO_PKG_VERSION"));

/// パッケージをDLして検証・有効化する。既に検証済みなら [`DownloadOutcome::AlreadyPresent`]。
pub fn download_package(options: &DownloadOptions<'_>) -> Result<DownloadOutcome, DownloadError> {
    if !options.repo_base.starts_with("http://") && !options.repo_base.starts_with("https://") {
        return Err(DownloadError::InvalidRepo(options.repo_base.to_string()));
    }
    // 呼び出し側が指定したファイル集合で検証 (manifest全体ではなく —
    // 部分取得の正当性はDL指示側の契約)。
    if crate::manifest::verify_files(&options.models_dir.join(options.model_id), options.files)
        .is_verified()
    {
        return Ok(DownloadOutcome::AlreadyPresent);
    }
    let total_bytes: u64 = options.files.values().map(|f| f.size_bytes).sum();
    let package_dir = options.models_dir.join(options.model_id);
    let staging_dir = options.models_dir.join(format!(
        ".staging-{}-{}",
        options.model_id,
        std::process::id()
    ));
    std::fs::create_dir_all(&staging_dir)
        .map_err(|e| DownloadError::Io(format!("staging作成失敗: {e}")))?;
    let result = download_into_staging(options, &staging_dir, total_bytes, &package_dir);
    match &result {
        Ok(_) => {}
        Err(_) => {
            // 失敗時はstagingを掃除 (既存モデルは維持 — models_dir/<model> は触っていない)
            let _ = std::fs::remove_dir_all(&staging_dir);
        }
    }
    result
}

fn download_into_staging(
    options: &DownloadOptions<'_>,
    staging_dir: &Path,
    total_bytes: u64,
    package_dir: &Path,
) -> Result<DownloadOutcome, DownloadError> {
    let client = build_client()?;
    // 空き容量の事前確認 (stagingの親 = models_dir)
    if let Some(available) = available_bytes(options.models_dir) {
        if available < total_bytes {
            return Err(DownloadError::DiskSpace {
                needed: total_bytes,
                available,
            });
        }
    }
    let mut completed_bytes: u64 = 0;
    let mut downloaded_bytes: u64 = 0;
    for (name, expected) in options.files {
        if options.cancel.load(Ordering::SeqCst) {
            return Err(DownloadError::Cancelled);
        }
        let part_path = staging_dir.join(format!("{name}.part"));
        let final_path = staging_dir.join(name);
        // staging内に検証済みファイルがあれば再利用 (再開DLの高速化)
        if final_path.is_file() {
            let size_ok = std::fs::metadata(&final_path)
                .map(|m| m.len() == expected.size_bytes)
                .unwrap_or(false);
            let ok = size_ok
                && file_sha256(&final_path)
                    .map(|h| h.eq_ignore_ascii_case(&expected.sha256))
                    .unwrap_or(false);
            if ok {
                completed_bytes += expected.size_bytes;
                continue;
            }
            let _ = std::fs::remove_file(&final_path);
        }
        // .partがあればRange再開を試みる
        let mut hasher = Sha256::new();
        let mut offset: u64 = 0;
        if part_path.is_file() {
            let mut part = std::fs::File::open(&part_path)
                .map_err(|e| DownloadError::Io(format!("part open失敗: {e}")))?;
            let mut buffer = [0u8; 256 * 1024];
            loop {
                let read = part
                    .read(&mut buffer)
                    .map_err(|e| DownloadError::Io(format!("part read失敗: {e}")))?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
                offset += read as u64;
            }
        }
        let mut request = client
            .get(format!("{}/{name}", options.repo_base))
            .header(reqwest::header::USER_AGENT, USER_AGENT);
        if offset > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
        }
        let response = request
            .send()
            .map_err(|e| DownloadError::Http(format!("{name} 取得失敗: {e}")))?;
        let status = response.status();
        let mut writer = if offset > 0 && status == reqwest::StatusCode::PARTIAL_CONTENT {
            // 再開: 206 — 既存partへ追記 (hashは.part全体の再計算済み)
            std::fs::OpenOptions::new()
                .append(true)
                .open(&part_path)
                .map_err(|e| DownloadError::Io(format!("part追記open失敗: {e}")))?
        } else {
            // 新規 (200)。Range非対応サーバーもここに落ちる — hash・offsetを
            // リセットして最初から書き直す
            if status != reqwest::StatusCode::OK {
                return Err(DownloadError::Http(format!(
                    "{name} の応答が不正です: {status}"
                )));
            }
            hasher = Sha256::new();
            offset = 0;
            std::fs::File::create(&part_path)
                .map_err(|e| DownloadError::Io(format!("part作成失敗: {e}")))?;
            std::fs::OpenOptions::new()
                .append(true)
                .open(&part_path)
                .map_err(|e| DownloadError::Io(format!("part open失敗: {e}")))?
        };
        let mut reader = response;
        let mut chunk = [0u8; 256 * 1024];
        loop {
            if options.cancel.load(Ordering::SeqCst) {
                return Err(DownloadError::Cancelled);
            }
            let read = reader
                .read(&mut chunk)
                .map_err(|e| DownloadError::Http(format!("{name} 読み取り失敗: {e}")))?;
            if read == 0 {
                break;
            }
            writer
                .write_all(&chunk[..read])
                .map_err(|e| DownloadError::Io(format!("{name} 書き込み失敗: {e}")))?;
            hasher.update(&chunk[..read]);
            offset += read as u64;
            downloaded_bytes += read as u64;
            (options.progress)(&DownloadProgress {
                file: name.clone(),
                file_bytes_done: offset.min(expected.size_bytes),
                file_bytes_total: expected.size_bytes,
                total_bytes_done: (completed_bytes + offset).min(total_bytes),
                total_bytes,
            });
        }
        drop(writer);
        // サイズ・sha256検証 → .partを確定名へ
        let actual_size = std::fs::metadata(&part_path)
            .map_err(|e| DownloadError::Io(format!("part metadata失敗: {e}")))?
            .len();
        if actual_size != expected.size_bytes {
            return Err(DownloadError::Verification {
                file: name.clone(),
                reason: format!(
                    "サイズ不一致: 実測 {actual_size} ≠ 期待 {}",
                    expected.size_bytes
                ),
            });
        }
        let actual_hash = hex(&hasher.finalize());
        if !actual_hash.eq_ignore_ascii_case(&expected.sha256) {
            return Err(DownloadError::Verification {
                file: name.clone(),
                reason: format!("sha256不一致: 実測 {actual_hash}"),
            });
        }
        std::fs::rename(&part_path, final_path)
            .map_err(|e| DownloadError::Io(format!("{name} 確定rename失敗: {e}")))?;
        completed_bytes += expected.size_bytes;
    }
    // 全ファイル検証済み → アトミックrenameで有効化
    if package_dir.exists() {
        // 破損パッケージが残っている場合のみ置換対象
        let _ = std::fs::remove_dir_all(package_dir);
    }
    std::fs::rename(staging_dir, package_dir)
        .map_err(|e| DownloadError::Io(format!("パッケージ有効化rename失敗: {e}")))?;
    Ok(DownloadOutcome::Downloaded {
        bytes: downloaded_bytes,
    })
}

fn file_sha256(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 256 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::ManifestFile;
    use std::collections::BTreeMap;
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::Arc;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "gento-embed-dl-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).expect("temp dir");
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// テスト用最小HTTPサーバー (1接続ずつhandle Fnで処理)。Range対応。
    struct MiniHttp {
        addr: std::net::SocketAddr,
    }

    type Handler = Arc<dyn Fn(&str, u64) -> (reqwest::StatusCode, Vec<u8>, bool) + Send + Sync>;

    impl MiniHttp {
        /// handle(path, range_offset) -> (status, body, _unused)
        fn start(body_of: Handler) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let addr = listener.local_addr().expect("addr");
            std::thread::Builder::new()
                .name("mini-http".into())
                .spawn(move || {
                    for stream in listener.incoming() {
                        let Ok(mut stream) = stream else { continue };
                        let handle = Arc::clone(&body_of);
                        std::thread::spawn(move || {
                            let mut buf = [0u8; 8192];
                            let Ok(n) = stream.read(&mut buf) else { return };
                            let request = String::from_utf8_lossy(&buf[..n]).to_string();
                            let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
                            let range = request
                                .lines()
                                .find(|l| l.to_ascii_lowercase().starts_with("range:"))
                                .and_then(|l| l.split(':').nth(1))
                                .and_then(|v| v.trim().strip_prefix("bytes="))
                                .and_then(|v| v.split('-').next())
                                .and_then(|v| v.parse::<u64>().ok())
                                .unwrap_or(0);
                            let (status, body, _) = handle(&path, range);
                            let mut response = format!(
                                "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
                                status.as_str(),
                                status.canonical_reason().unwrap_or(""),
                                body.len()
                            )
                            .into_bytes();
                            if status == reqwest::StatusCode::PARTIAL_CONTENT && range > 0 {
                                let total = range + body.len() as u64;
                                let head = format!(
                                    "Content-Range: bytes {range}-{}/{total}\r\n",
                                    total - 1
                                );
                                response.extend_from_slice(head.as_bytes());
                            }
                            response.extend_from_slice(b"\r\n");
                            response.extend_from_slice(&body);
                            let _ = stream.write_all(&response);
                            let _ = stream.flush();
                        });
                    }
                })
                .expect("spawn");
            Self { addr }
        }

        fn base(&self) -> String {
            format!("http://{}", self.addr)
        }
    }

    fn files_map(entries: &[(&str, &str, u64)]) -> BTreeMap<String, ManifestFile> {
        entries
            .iter()
            .map(|(name, hash, size)| {
                (
                    name.to_string(),
                    ManifestFile {
                        sha256: hash.to_string(),
                        size_bytes: *size,
                    },
                )
            })
            .collect()
    }

    fn sha256_hex(data: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(data);
        hex(&hasher.finalize())
    }

    #[test]
    fn rejects_non_http_repo_base() {
        let dir = TempDir::new("invalid");
        let files = files_map(&[]);
        let cancel = AtomicBool::new(false);
        let result = download_package(&DownloadOptions {
            repo_base: "ftp://example.com/models",
            model_id: "ruri-v3-30m",
            models_dir: dir.path(),
            files: &files,
            progress: &|_| {},
            cancel: &cancel,
        });
        assert!(
            matches!(result, Err(DownloadError::InvalidRepo(_))),
            "{result:?}"
        );
    }

    #[test]
    fn downloads_verifies_and_activates() {
        let dir = TempDir::new("ok");
        let model = b"fake onnx weights for test";
        let notice = b"test notice";
        let files = files_map(&[
            ("model.onnx", &sha256_hex(model), model.len() as u64),
            ("NOTICE", &sha256_hex(notice), notice.len() as u64),
        ]);
        let model_owned = model.to_vec();
        let notice_owned = notice.to_vec();
        let server = MiniHttp::start(Arc::new(move |path: &str, offset: u64| {
            let body = if path.ends_with("model.onnx") {
                model_owned.clone()
            } else {
                notice_owned.clone()
            };
            if offset > 0 && offset < body.len() as u64 {
                return (
                    reqwest::StatusCode::PARTIAL_CONTENT,
                    body[offset as usize..].to_vec(),
                    true,
                );
            }
            (reqwest::StatusCode::OK, body, true)
        }));
        let cancel = AtomicBool::new(false);
        let progress_count = std::cell::Cell::new(0usize);
        let result = download_package(&DownloadOptions {
            repo_base: &server.base(),
            model_id: "ruri-v3-30m",
            models_dir: dir.path(),
            files: &files,
            progress: &|_| progress_count.set(progress_count.get() + 1),
            cancel: &cancel,
        });
        match &result {
            Ok(DownloadOutcome::Downloaded { bytes }) => {
                assert!(*bytes >= (model.len() + notice.len()) as u64);
            }
            other => panic!("期待と違う結果: {other:?}"),
        }
        assert!(progress_count.get() > 0, "進捗コールバックが呼ばれた");
        // 有効化検証
        assert_eq!(
            crate::verify_package_with_spec(
                &crate::manifest::ModelSpec {
                    model_id: "ruri-v3-30m".into(),
                    package_name: "t".into(),
                    package_version: 1,
                    repository_id: "t".into(),
                    revision: "t".into(),
                    license: "t".into(),
                    embedding_dim: 1,
                    max_input_length: 1,
                    query_prefix: String::new(),
                    document_prefix: String::new(),
                    files: files.clone(),
                },
                &dir.path().join("ruri-v3-30m")
            ),
            crate::manifest::PackageVerification::Verified
        );
        // stagingは残っていない
        let entries: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(entries.len(), 1, "stagingは掃除されている");
        // 2回目はAlreadyPresent
        let cancel2 = AtomicBool::new(false);
        let result2 = download_package(&DownloadOptions {
            repo_base: &server.base(),
            model_id: "ruri-v3-30m",
            models_dir: dir.path(),
            files: &files,
            progress: &|_| {},
            cancel: &cancel2,
        });
        assert!(matches!(result2, Ok(DownloadOutcome::AlreadyPresent)));
    }

    #[test]
    fn hash_mismatch_keeps_package_unactivated() {
        let dir = TempDir::new("mismatch");
        let model = b"will be corrupted in transit";
        let files = files_map(&[(
            "model.onnx",
            "0000000000000000000000000000000000000000000000000000000000000000",
            model.len() as u64,
        )]);
        let model_owned = model.to_vec();
        let server = MiniHttp::start(Arc::new(move |_, _| {
            (reqwest::StatusCode::OK, model_owned.clone(), true)
        }));
        let cancel = AtomicBool::new(false);
        let result = download_package(&DownloadOptions {
            repo_base: &server.base(),
            model_id: "ruri-v3-30m",
            models_dir: dir.path(),
            files: &files,
            progress: &|_| {},
            cancel: &cancel,
        });
        match &result {
            Err(DownloadError::Verification { file, .. }) => assert_eq!(file, "model.onnx"),
            other => panic!("検証失敗になるべき: {other:?}"),
        }
        assert!(
            !dir.path().join("ruri-v3-30m").exists(),
            "有効化されていない"
        );
        let entries: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(entries.len(), 0, "stagingも掃除されている");
    }

    #[test]
    fn cancel_stops_download() {
        let dir = TempDir::new("cancel");
        let big = vec![0xABu8; 4 * 1024 * 1024];
        let files = files_map(&[("model.onnx", &sha256_hex(&big), big.len() as u64)]);
        let big_arc = Arc::new(big);
        let server = MiniHttp::start(Arc::new(move |_, _| {
            // 少しずつ遅延させてキャンセル観測の機会を作る
            std::thread::sleep(Duration::from_millis(5));
            (reqwest::StatusCode::OK, (*big_arc).clone(), true)
        }));
        let cancel = AtomicBool::new(false);
        let cancel_ref = &cancel;
        let result = download_package(&DownloadOptions {
            repo_base: &server.base(),
            model_id: "ruri-v3-30m",
            models_dir: dir.path(),
            files: &files,
            progress: &|p| {
                if p.file_bytes_done > 1024 * 1024 {
                    cancel_ref.store(true, Ordering::SeqCst);
                }
            },
            cancel: &cancel,
        });
        assert!(matches!(result, Err(DownloadError::Cancelled)));
        assert!(!dir.path().join("ruri-v3-30m").exists());
    }

    #[test]
    fn resume_from_partial_part_file() {
        // .partが途中まである状態からRange再開 (206) で完走する。
        // download_into_staging を直接駆動してstagingを制御する。
        let dir = TempDir::new("resume");
        let big = vec![7u8; 300_000];
        let hash = sha256_hex(&big);
        let files = files_map(&[("model.onnx", &hash, big.len() as u64)]);
        let staging = dir.path().join("staging-custom");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("model.onnx.part"), &big[..100_000]).unwrap();
        let ranges_seen: Arc<std::sync::Mutex<Vec<u64>>> = Arc::default();
        let big_arc = Arc::new(big);
        let ranges_record = Arc::clone(&ranges_seen);
        let server = MiniHttp::start(Arc::new(move |_, offset: u64| {
            ranges_record.lock().unwrap().push(offset);
            let body = big_arc.clone();
            if offset > 0 {
                (
                    reqwest::StatusCode::PARTIAL_CONTENT,
                    body[offset as usize..].to_vec(),
                    true,
                )
            } else {
                (reqwest::StatusCode::OK, body.to_vec(), true)
            }
        }));
        let cancel = AtomicBool::new(false);
        let total: u64 = files.values().map(|f| f.size_bytes).sum();
        let package_dir = dir.path().join("ruri-v3-30m");
        let result = download_into_staging(
            &DownloadOptions {
                repo_base: &server.base(),
                model_id: "ruri-v3-30m",
                models_dir: dir.path(),
                files: &files,
                progress: &|_| {},
                cancel: &cancel,
            },
            &staging,
            total,
            &package_dir,
        );
        assert!(result.is_ok(), "{result:?}");
        assert!(package_dir.join("model.onnx").exists());
        let seen = ranges_seen.lock().unwrap().clone();
        assert!(
            seen.contains(&100_000),
            "Range: bytes=100000- が送信された: {seen:?}"
        );
    }

    #[test]
    fn available_bytes_returns_positive_for_existing_dir() {
        let dir = TempDir::new("statvfs");
        match available_bytes(dir.path()) {
            Some(bytes) => assert!(bytes > 0, "空き容量は正"),
            None => {
                // 非Linux環境ではskip
            }
        }
        // 存在しない親の深いパスでもfail-open (Noneまたは正の値)
        let _ = available_bytes(&dir.path().join("nested/deep"));
    }
}
