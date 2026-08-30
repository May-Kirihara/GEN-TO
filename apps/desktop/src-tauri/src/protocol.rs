//! ローカル画像プロトコル (Phase 5 / WP-4)。
//!
//! セキュリティ契約 (docs/plans/2026-08-28-spike-implementation-pm.md §4.1):
//! - URLのどの部分もファイルシステムパスとして解釈しない。解決は
//!   アプリ保有レジストリ (AssetId → カタログレコード → キャッシュキー/元パス) のみ
//! - 厳密検証: scheme・authority (host/userinfo/port)・メソッド (GETのみ)・
//!   セグメント数と順序・ID構文 (10進・20桁上限)・サイズクラスenum完全一致・
//!   クエリ (`v` のみ・10進・18桁上限)。未知クエリ・過剰セグメントは拒否
//! - パーセントエンコード分離子・二重エンコード・バックスラッシュ・NUL・`..` を拒否
//!   (url crateの構造化パース + 生パスセグメントのASCII厳密一致で保証)
//! - キャッシュは正規化ルート包含 + シンボリックリンク拒否 (最終コンポーネントは
//!   `symlink_metadata` で正規ファイル確認) + サイズ上限 (サムネイル64MiB / 元512MiB)
//! - 読み取りは検証とopenの間の差し替え (TOCTOU) を閉じる: 最終コンポーネントを
//!   `O_NOFOLLOW` で開き、open済みfdのfstatで検証時statの (dev, ino) 一致と
//!   正規キャッシュルート包含 (`/proc/self/fd/{fd}` の実パス) を再確認する
//! - 応答は `Content-Type` 正確・`X-Content-Type-Options: nosniff`・意図的な
//!   `Cache-Control` (`v` 付きは immutable、無しは no-store)。
//!   エラー本文・ログにファイルパスを出さない (IDとステータスのみ)
//! - 未生成は生成を待たず即時404 (スクロール中の大量要求を滞留させない)
//! - プロトコルハンドラは非同期登録し、ファイル読み込みは
//!   `tauri::async_runtime::spawn_blocking` で実行する
//!
//! URL契約 (docs/dev/api-contract.md §12):
//! - `gento-thumb://localhost/asset/{id}/{s256|s512}?v={version}`
//! - `gento-preview://localhost/asset/{id}?v={version}`
//! - `gento-original://localhost/asset/{id}?v={version}` (登録済みカタログ資産の元PNG)
//!
//! `v` は内容感応バージョン (FG-1): 元ファイルのpath+size+mtimeとアルゴリズム
//! バージョンから決まる (gento_image::cache::content_version)。同一URLは
//! 同一内容を意味するためimmutable応答が安全になる。

use std::fs::{self, File};
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use gento_core::catalog::ImageState;
use gento_core::jobs::SizeClass;
use gento_core::AssetId;
use gento_image::cache::{asset_fingerprint, cache_key, CacheLookup, ThumbnailCache};
use gento_image::stats::percentile;
use tauri::http::{self, StatusCode};
use tauri::Url;
use tauri::{Manager, Runtime, UriSchemeContext, UriSchemeResponder};

use crate::catalog::AppState;
use crate::thumbnails::AssetSnapshot;

/// AssetIdセグメントの桁数上限 (u64最大値が20桁のため)。
pub const MAX_ID_DIGITS: usize = 20;
/// クエリ `v` (cache generation) の桁数上限。
pub const MAX_GEN_DIGITS: usize = 18;
/// サムネイル/previewキャッシュファイルのサイズ上限。
pub const MAX_THUMB_BYTES: u64 = 64 * 1024 * 1024;
/// 元ファイル (gento-original) のサイズ上限。
pub const MAX_ORIGINAL_BYTES: u64 = 512 * 1024 * 1024;

/// `v` 付きURLのキャッシュ指示 (同一URLの内容は不変)。
const CACHE_CONTROL_IMMUTABLE: &str = "public, max-age=31536000, immutable";
/// `v` 無しURLのキャッシュ指示 (WebViewキャッシュに残さない)。
const CACHE_CONTROL_NO_STORE: &str = "no-store";

/// プロトコル種別 (URLスキームと1:1)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtoKind {
    Thumb,
    Preview,
    Original,
}

impl ProtoKind {
    pub const fn scheme(self) -> &'static str {
        match self {
            ProtoKind::Thumb => "gento-thumb",
            ProtoKind::Preview => "gento-preview",
            ProtoKind::Original => "gento-original",
        }
    }
}

/// 検証済み要求。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedRequest {
    pub kind: ProtoKind,
    pub asset_id: AssetId,
    /// Thumbスキームのみ (S256/S512)。Preview/OriginalはNone。
    pub size_class: Option<SizeClass>,
    /// クエリ `v` (cache generation)。不在の場合 None。
    pub cache_gen: Option<u64>,
}

/// 検証拒否。理由kindはログ用の固定文字列で、入力URLは含まない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    MethodNotAllowed,
    Malformed(&'static str),
}

impl Reject {
    pub const fn reason(self) -> &'static str {
        match self {
            Reject::MethodNotAllowed => "method_not_allowed",
            Reject::Malformed(reason) => reason,
        }
    }

    const fn status(self) -> StatusCode {
        match self {
            Reject::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            Reject::Malformed(_) => StatusCode::BAD_REQUEST,
        }
    }
}

/// URLを構造化パースして厳密検証する (メソッド・IOなしの純粋関数)。
///
/// 生の (パーセントエンコードを保持した) パスセグメントをASCII厳密一致で
/// 検証するため、`%2F` `%2E` `%5C` `%00` 等のエンコード分離子はすべて
/// 「正当な構文に一致しない」ものとして拒否される。
pub fn parse_protocol_request(
    kind: ProtoKind,
    method: &http::Method,
    raw_uri: &str,
) -> Result<ResolvedRequest, Reject> {
    if method != http::Method::GET {
        return Err(Reject::MethodNotAllowed);
    }
    let url = Url::parse(raw_uri).map_err(|_| Reject::Malformed("unparseable"))?;
    if url.scheme() != kind.scheme() {
        return Err(Reject::Malformed("scheme"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Reject::Malformed("userinfo"));
    }
    if url.port().is_some() {
        return Err(Reject::Malformed("port"));
    }
    // host_str: Domainはそのまま、IPv4は "1.2.3.4"、IPv6は "[::1]" (括弧付き)。
    // "localhost" 完全一致のみ許可するため、IP形式はすべて拒否される。
    match url.host_str() {
        None | Some("localhost") => {}
        Some(_) => return Err(Reject::Malformed("host")),
    }
    if url.fragment().is_some() {
        return Err(Reject::Malformed("fragment"));
    }
    let Some(segments) = url.path_segments() else {
        // cannot-be-a-base (authorityのないopaque形式)
        return Err(Reject::Malformed("opaque"));
    };
    let segments: Vec<&str> = segments.collect();
    if segments.first() != Some(&"asset") {
        return Err(Reject::Malformed("asset_prefix"));
    }
    let Some(&id_seg) = segments.get(1) else {
        return Err(Reject::Malformed("segments"));
    };
    if id_seg.is_empty()
        || id_seg.len() > MAX_ID_DIGITS
        || !id_seg.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(Reject::Malformed("id"));
    }
    let asset_id = AssetId::new(id_seg.parse::<u64>().map_err(|_| Reject::Malformed("id"))?);
    let size_class = match kind {
        ProtoKind::Thumb => {
            let Some(&class_seg) = segments.get(2) else {
                return Err(Reject::Malformed("segments"));
            };
            match class_seg {
                "s256" => Some(SizeClass::S256),
                "s512" => Some(SizeClass::S512),
                _ => return Err(Reject::Malformed("size_class")),
            }
        }
        ProtoKind::Preview | ProtoKind::Original => None,
    };
    let expected_segments = if matches!(kind, ProtoKind::Thumb) {
        3
    } else {
        2
    };
    if segments.len() != expected_segments {
        return Err(Reject::Malformed("segments"));
    }
    let cache_gen = match url.query() {
        None => None,
        Some(query) => {
            let Some(digits) = query.strip_prefix("v=") else {
                return Err(Reject::Malformed("query"));
            };
            if digits.is_empty()
                || digits.len() > MAX_GEN_DIGITS
                || !digits.bytes().all(|b| b.is_ascii_digit())
            {
                return Err(Reject::Malformed("query"));
            }
            Some(
                digits
                    .parse::<u64>()
                    .map_err(|_| Reject::Malformed("query"))?,
            )
        }
    };
    Ok(ResolvedRequest {
        kind,
        asset_id,
        size_class,
        cache_gen,
    })
}

/// ファイル解決の結果。
pub struct ServeSpec {
    pub path: PathBuf,
    pub content_type: &'static str,
    pub max_bytes: u64,
    pub cache_control: &'static str,
    /// 検証時 (`resolve_file` 内の `symlink_metadata`) の (dev, ino)。
    /// open後のfstat一致検証に使う (検証とopen間の差し替え検出)。
    pub file_id: (u64, u64),
    /// open済みfdの実パス包含を再検証する正規キャッシュルート。
    /// None = 元ファイル系 (`gento-original`。包含対象なし、fstat一致のみ)。
    pub canonical_root: Option<PathBuf>,
}

/// 解決失敗。パス等の詳細は保持しない (ログにIDとステータスのみ出す契約)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveFailure {
    NotFound,
    TooLarge,
}

/// 検証済み要求をレジストリ (アセットスナップショット + キャッシュ) のみで解決する。
/// 未生成 (キャッシュミス) は生成を待たず [`ResolveFailure::NotFound`]。
pub fn resolve_file(
    req: &ResolvedRequest,
    asset: &AssetSnapshot,
    cache: &ThumbnailCache,
) -> Result<ServeSpec, ResolveFailure> {
    let cache_control = match req.cache_gen {
        Some(_) => CACHE_CONTROL_IMMUTABLE,
        None => CACHE_CONTROL_NO_STORE,
    };
    match req.kind {
        ProtoKind::Thumb | ProtoKind::Preview => {
            let size_class = match (req.kind, req.size_class) {
                (ProtoKind::Thumb, Some(class)) => class,
                // parse済み要求ではThumbは必ずsize_classを持つ
                (ProtoKind::Preview, _) => SizeClass::Preview2048,
                (ProtoKind::Thumb, None) | (ProtoKind::Original, _) => {
                    return Err(ResolveFailure::NotFound)
                }
            };
            let fingerprint =
                asset_fingerprint(&asset.source_path, asset.byte_size, asset.mtime_ms);
            let key = cache_key(&fingerprint, size_class);
            let CacheLookup::Hit(path) = cache.lookup(&key) else {
                // 未生成: 生成を待ってブロックしない (即時404)
                return Err(ResolveFailure::NotFound);
            };
            // 最終コンポーネントはシンボリックリンクでない正規ファイルのみ
            let md = fs::symlink_metadata(&path).map_err(|_| ResolveFailure::NotFound)?;
            if !md.file_type().is_file() {
                return Err(ResolveFailure::NotFound);
            }
            if md.len() > MAX_THUMB_BYTES {
                return Err(ResolveFailure::TooLarge);
            }
            // キャッシュルート配下への包含 (中間ディレクトリのsymlink脱出対策)。
            // 正規化ルートを返し、open後の再検証に使う。
            let canonical_root = verify_within_cache_root(&path, cache.root())?;
            Ok(ServeSpec {
                path,
                content_type: "image/webp",
                max_bytes: MAX_THUMB_BYTES,
                cache_control,
                file_id: (md.dev(), md.ino()),
                canonical_root: Some(canonical_root),
            })
        }
        ProtoKind::Original => {
            if !asset.readable {
                return Err(ResolveFailure::NotFound);
            }
            // 登録済みカタログ資産の元ファイル。シンボリックリンク追従を拒否する
            let md =
                fs::symlink_metadata(&asset.source_path).map_err(|_| ResolveFailure::NotFound)?;
            if !md.file_type().is_file() {
                return Err(ResolveFailure::NotFound);
            }
            if md.len() > MAX_ORIGINAL_BYTES {
                return Err(ResolveFailure::TooLarge);
            }
            Ok(ServeSpec {
                path: asset.source_path.clone(),
                content_type: "image/png",
                max_bytes: MAX_ORIGINAL_BYTES,
                cache_control,
                file_id: (md.dev(), md.ino()),
                canonical_root: None,
            })
        }
    }
}

/// 最終コンポーネントがシンボリックリンクでないことを symlink_metadata で確認し、
/// canonicalize したキャッシュルート配下に収まることを検証する。
/// 戻り値は正規化済みキャッシュルート (open後の包含再検証用)。
fn verify_within_cache_root(path: &Path, root: &Path) -> Result<PathBuf, ResolveFailure> {
    let canonical = fs::canonicalize(path).map_err(|_| ResolveFailure::NotFound)?;
    let canonical_root = fs::canonicalize(root).map_err(|_| ResolveFailure::NotFound)?;
    if canonical.starts_with(&canonical_root) {
        Ok(canonical_root)
    } else {
        Err(ResolveFailure::NotFound)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadError {
    Gone,
    Io,
}

/// 解決済みパスを読み込む。`resolve_file` での検証とopenの間の差し替え
/// (TOCTOU) を閉じる:
///
/// 1. 最終コンポーネントを `O_NOFOLLOW` 付きで開く (シンボリックリンクなら
///    ELOOPで失敗 = 検証後のシンボリックリンク差し替えを拒否)
/// 2. open済みfdをfstatし、(a) 正規ファイル (b) サイズ上限 (c) 検証時statの
///    (dev, ino) 一致を確認する (中間ディレクトリ差し替えで別ファイルが開けば
///    不一致となり拒否)
/// 3. キャッシュ系はfdの実パス (`/proc/self/fd/{fd}` のreadlink) が正規
///    キャッシュルート配下であることを再確認する (防御の重ね)
///
/// open後のreadはfdがinodeをピンするため、以後の差し替え・renameの影響を受けない。
///
/// 残る理論的制限: 同一inodeへのhardlink差し替えは (dev, ino) と実パスの
/// いずれでも区別できない。キャッシュディレクトリへ書き込める攻撃者は
/// ファイル内容を直接書き換えることもできるため脅威モデル外とする
/// (キャッシュディレクトリはアプリ私有)。
fn read_file_checked(spec: &ServeSpec) -> Result<Vec<u8>, ReadError> {
    let file = File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&spec.path)
        .map_err(|e| match e.raw_os_error() {
            // O_NOFOLLOW: 最終コンポーネントがシンボリックリンク (検証後の
            // 差し替え)。消失 (ENOENT) と同じ「もう無い」扱い。
            Some(libc::ELOOP) | Some(libc::ENOENT) | None => ReadError::Gone,
            _ => ReadError::Io,
        })?;
    let md = file.metadata().map_err(|_| ReadError::Gone)?;
    if !md.is_file() || md.len() > spec.max_bytes {
        return Err(ReadError::Gone);
    }
    if (md.dev(), md.ino()) != spec.file_id {
        // 検証時と別のinode = 中間ディレクトリ差し替え等
        return Err(ReadError::Gone);
    }
    if let Some(root) = &spec.canonical_root {
        let proc_path = PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()));
        let real = fs::read_link(&proc_path).map_err(|_| ReadError::Io)?;
        // open済みパスがrename/unlink済みならcanonicalize失敗 → 拒否 (安全側)
        let real = fs::canonicalize(&real).map_err(|_| ReadError::Gone)?;
        if !real.starts_with(root) {
            return Err(ReadError::Gone);
        }
    }
    let mut buf = Vec::with_capacity(md.len() as usize);
    file.take(spec.max_bytes.saturating_add(1))
        .read_to_end(&mut buf)
        .map_err(|_| ReadError::Io)?;
    if buf.len() as u64 != md.len() {
        return Err(ReadError::Gone);
    }
    Ok(buf)
}

/// 検証済み要求を解決して応答へ組み立てる (ハンドラの spawn_blocking 内・単体テスト両方)。
pub fn serve_resolved(
    req: &ResolvedRequest,
    asset: Option<&AssetSnapshot>,
    cache: Option<&ThumbnailCache>,
) -> http::Response<Vec<u8>> {
    let Some(asset) = asset else {
        tracing::debug!(
            scheme = req.kind.scheme(),
            id = req.asset_id.value(),
            status = 404u16,
            "protocol asset not found"
        );
        return error_response(StatusCode::NOT_FOUND);
    };
    let Some(cache) = cache else {
        tracing::debug!(
            scheme = req.kind.scheme(),
            id = req.asset_id.value(),
            status = 503u16,
            "protocol cache unavailable"
        );
        return error_response(StatusCode::SERVICE_UNAVAILABLE);
    };
    match resolve_file(req, asset, cache) {
        Ok(spec) => match read_file_checked(&spec) {
            Ok(body) => ok_response(&spec, body),
            Err(ReadError::Gone) => {
                tracing::debug!(
                    scheme = req.kind.scheme(),
                    id = req.asset_id.value(),
                    status = 404u16,
                    "protocol file gone"
                );
                error_response(StatusCode::NOT_FOUND)
            }
            Err(ReadError::Io) => {
                // io::ErrorのDisplayはstdではパスを含まないが、種別のみ記録する
                tracing::warn!(
                    scheme = req.kind.scheme(),
                    id = req.asset_id.value(),
                    status = 500u16,
                    "protocol read failed"
                );
                error_response(StatusCode::INTERNAL_SERVER_ERROR)
            }
        },
        Err(ResolveFailure::NotFound) => {
            tracing::debug!(
                scheme = req.kind.scheme(),
                id = req.asset_id.value(),
                status = 404u16,
                "protocol resolve not found"
            );
            error_response(StatusCode::NOT_FOUND)
        }
        Err(ResolveFailure::TooLarge) => {
            tracing::debug!(
                scheme = req.kind.scheme(),
                id = req.asset_id.value(),
                status = 413u16,
                "protocol resolve too large"
            );
            error_response(StatusCode::PAYLOAD_TOO_LARGE)
        }
    }
}

/// 要求全体 (メソッド検証 → URL検証 → 解決 → 応答) を処理する。
/// Tauri stateに触れない純粋な関数で、単体テストの入り口。
pub fn process_request(
    kind: ProtoKind,
    method: &http::Method,
    raw_uri: &str,
    asset: Option<&AssetSnapshot>,
    cache: Option<&ThumbnailCache>,
) -> http::Response<Vec<u8>> {
    match parse_protocol_request(kind, method, raw_uri) {
        Ok(req) => serve_resolved(&req, asset, cache),
        Err(reject) => error_response(reject.status()),
    }
}

fn ok_response(spec: &ServeSpec, body: Vec<u8>) -> http::Response<Vec<u8>> {
    response_with(StatusCode::OK, spec.content_type, spec.cache_control, body)
}

fn error_response(status: StatusCode) -> http::Response<Vec<u8>> {
    let body: &str = match status {
        StatusCode::METHOD_NOT_ALLOWED => "method not allowed",
        StatusCode::BAD_REQUEST => "bad request",
        StatusCode::NOT_FOUND => "not found",
        StatusCode::PAYLOAD_TOO_LARGE => "too large",
        StatusCode::INTERNAL_SERVER_ERROR => "internal error",
        StatusCode::SERVICE_UNAVAILABLE => "unavailable",
        _ => "error",
    };
    response_with(
        status,
        "text/plain; charset=utf-8",
        CACHE_CONTROL_NO_STORE,
        body.as_bytes().to_vec(),
    )
}

fn response_with(
    status: StatusCode,
    content_type: &str,
    cache_control: &str,
    body: Vec<u8>,
) -> http::Response<Vec<u8>> {
    http::Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, content_type)
        .header(http::header::CONTENT_LENGTH, body.len().to_string())
        .header("X-Content-Type-Options", "nosniff")
        .header(http::header::CACHE_CONTROL, cache_control)
        .body(body)
        .expect("statically valid response parts")
}

/// プロトコル応答の統計 (WP-4。「応答時間と転送量を記録する」)。
#[derive(Debug, Default)]
pub struct ProtoStats {
    requests: AtomicU64,
    ok: AtomicU64,
    bytes_sent: AtomicU64,
    /// 応答時間 (ミリ秒)。上限まで蓄積し、以降は数値のみ更新する。
    durations_ms: Mutex<Vec<f64>>,
}

/// 統計サンプル数の上限 (高速スクロールで無制限に増やさない)。
const MAX_DURATION_SAMPLES: usize = 100_000;

/// [`ProtoStats`] のスナップショット (`get_thumbnail_stats` 統合用)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProtoStatsSnapshot {
    pub requests: u64,
    pub ok: u64,
    pub bytes_sent: u64,
    pub p50_ms: Option<f64>,
    pub p95_ms: Option<f64>,
    pub p99_ms: Option<f64>,
}

impl ProtoStats {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, ok: bool, bytes: u64, duration_ms: f64) {
        self.requests.fetch_add(1, Ordering::Relaxed);
        if ok {
            self.ok.fetch_add(1, Ordering::Relaxed);
            self.bytes_sent.fetch_add(bytes, Ordering::Relaxed);
        }
        if let Ok(mut durations) = self.durations_ms.lock() {
            if durations.len() < MAX_DURATION_SAMPLES {
                durations.push(duration_ms);
            }
        }
    }

    pub fn snapshot(&self) -> ProtoStatsSnapshot {
        let durations = self
            .durations_ms
            .lock()
            .map(|d| d.clone())
            .unwrap_or_default();
        ProtoStatsSnapshot {
            requests: self.requests.load(Ordering::Relaxed),
            ok: self.ok.load(Ordering::Relaxed),
            bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
            p50_ms: percentile(&durations, 0.50),
            p95_ms: percentile(&durations, 0.95),
            p99_ms: percentile(&durations, 0.99),
        }
    }
}

/// 3つのローカル画像プロトコルをBuilderへ非同期登録する。
pub fn register_uri_scheme_protocols<R: Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder
        .register_asynchronous_uri_scheme_protocol(
            ProtoKind::Thumb.scheme(),
            move |ctx, request, responder| {
                handle_protocol_request(ProtoKind::Thumb, ctx, request, responder)
            },
        )
        .register_asynchronous_uri_scheme_protocol(
            ProtoKind::Preview.scheme(),
            move |ctx, request, responder| {
                handle_protocol_request(ProtoKind::Preview, ctx, request, responder)
            },
        )
        .register_asynchronous_uri_scheme_protocol(
            ProtoKind::Original.scheme(),
            move |ctx, request, responder| {
                handle_protocol_request(ProtoKind::Original, ctx, request, responder)
            },
        )
}

fn handle_protocol_request<R: Runtime>(
    kind: ProtoKind,
    ctx: UriSchemeContext<'_, R>,
    request: http::Request<Vec<u8>>,
    responder: UriSchemeResponder,
) {
    let started = Instant::now();
    let method = request.method().clone();
    let uri = request.uri().to_string();
    // 到着URL形式の実測用 (Linuxでhost付き `scheme://localhost/...` として届くかの確認。
    // debugレベルのみで、既定のinfoログには出さない)
    tracing::debug!(scheme = kind.scheme(), method = %method, uri = %uri, "protocol request arrived");

    let Some(state) = ctx.app_handle().try_state::<AppState>() else {
        // setup完了前 (state未管理)。即座に503。
        respond_with_stats(
            responder,
            error_response(StatusCode::SERVICE_UNAVAILABLE),
            &started,
            None,
        );
        return;
    };

    let parsed = parse_protocol_request(kind, &method, &uri);
    let req = match parsed {
        Ok(req) => req,
        Err(reject) => {
            // 検証拒否はIO不要のため即応答。ログは理由kindのみ (URL・パスを出さない)。
            tracing::debug!(
                scheme = kind.scheme(),
                reason = reject.reason(),
                "protocol request rejected"
            );
            respond_with_stats(
                responder,
                error_response(reject.status()),
                &started,
                Some(&state.proto_stats),
            );
            return;
        }
    };

    // カタログスナップショット (短時間のロックのみ。IOはspawn_blockingへ)。
    let snapshot = {
        let inner = state.lock_catalog();
        inner.record(req.asset_id).map(|record| AssetSnapshot {
            id: record.id,
            source_path: record.source_path.clone(),
            byte_size: record.byte_size,
            mtime_ms: record.mtime_ms,
            readable: record.state == ImageState::Ready,
        })
    };
    let cache = state
        .thumbnails
        .as_ref()
        .map(|service| Arc::clone(service.cache()));
    let stats = Arc::clone(&state.proto_stats);

    // プロトコル/UIスレッドをブロックしない: ファイルIOはブロッキングプールへ。
    // JoinHandleは即座に手放す (応答はresponder経由で返る)。
    let _handle = tauri::async_runtime::spawn_blocking(move || {
        let response = serve_resolved(&req, snapshot.as_ref(), cache.as_deref());
        respond_with_stats(responder, response, &started, Some(&stats));
    });
}

fn respond_with_stats(
    responder: UriSchemeResponder,
    response: http::Response<Vec<u8>>,
    started: &Instant,
    stats: Option<&ProtoStats>,
) {
    let ok = response.status().is_success();
    let bytes = response.body().len() as u64;
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    if let Some(stats) = stats {
        stats.record(ok, bytes, elapsed_ms);
    }
    responder.respond(response);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    static DIR_SEQ: AtomicU32 = AtomicU32::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let seq = DIR_SEQ.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "gento-proto-test-{}-{}-{}",
                name,
                std::process::id(),
                seq
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn get() -> http::Method {
        http::Method::GET
    }

    /// 検証拒否の理由kindを返す (期待値との厳密一致でテストする)。
    fn reject_reason(kind: ProtoKind, method: &http::Method, uri: &str) -> &'static str {
        parse_protocol_request(kind, method, uri)
            .expect_err("expected rejection")
            .reason()
    }

    fn parse_ok(kind: ProtoKind, uri: &str) -> ResolvedRequest {
        parse_protocol_request(kind, &get(), uri).expect("expected acceptance")
    }

    // ===== 正常系 (5種) =====

    #[test]
    fn accepts_thumb_localhost_with_generation() {
        let req = parse_ok(
            ProtoKind::Thumb,
            "gento-thumb://localhost/asset/42/s256?v=3",
        );
        assert_eq!(req.kind, ProtoKind::Thumb);
        assert_eq!(req.asset_id, AssetId::new(42));
        assert_eq!(req.size_class, Some(SizeClass::S256));
        assert_eq!(req.cache_gen, Some(3));
    }

    #[test]
    fn accepts_thumb_s512_without_query() {
        let req = parse_ok(ProtoKind::Thumb, "gento-thumb://localhost/asset/7/s512");
        assert_eq!(req.size_class, Some(SizeClass::S512));
        assert_eq!(req.cache_gen, None);
    }

    #[test]
    fn accepts_thumb_without_authority() {
        // hostが空 (authority不在) の形式も許可する
        let req = parse_ok(ProtoKind::Thumb, "gento-thumb:/asset/1/s256?v=1");
        assert_eq!(req.asset_id, AssetId::new(1));
    }

    #[test]
    fn accepts_preview_and_original_with_max_generation() {
        let gen = "9".repeat(18);
        let req = parse_ok(
            ProtoKind::Preview,
            &format!("gento-preview://localhost/asset/5?v={gen}"),
        );
        assert_eq!(req.size_class, None);
        assert_eq!(req.cache_gen, Some(999_999_999_999_999_999));
        let req = parse_ok(
            ProtoKind::Original,
            "gento-original://localhost/asset/5?v=1",
        );
        assert_eq!(req.size_class, None);
        assert_eq!(req.cache_gen, Some(1));
    }

    #[test]
    fn rejects_uppercase_localhost_host() {
        // 非特殊schemeのhostはopaque扱い (WHATWG) のため大文字は小文字化されない。
        // 厳密一致のみ許可する (フロントエンドビルダは常に小文字を生成する)。
        assert_eq!(
            reject_reason(
                ProtoKind::Thumb,
                &get(),
                "gento-thumb://LOCALHOST/asset/1/s256"
            ),
            "host"
        );
    }

    // ===== メソッド検証 =====

    #[test]
    fn rejects_head_and_post_with_405() {
        for method in [http::Method::HEAD, http::Method::POST] {
            let err = parse_protocol_request(
                ProtoKind::Thumb,
                &method,
                "gento-thumb://localhost/asset/1/s256",
            )
            .expect_err("expected rejection");
            assert_eq!(err, Reject::MethodNotAllowed);
            assert_eq!(err.status(), StatusCode::METHOD_NOT_ALLOWED);
        }
    }

    // ===== 拒否系 (検証行列) =====

    #[test]
    fn rejects_scheme_mismatch() {
        // thumbハンドラにpreviewスキームのURLが届いた場合
        assert_eq!(
            reject_reason(
                ProtoKind::Thumb,
                &get(),
                "gento-preview://localhost/asset/1/s256"
            ),
            "scheme"
        );
    }

    #[test]
    fn rejects_unparseable_uri() {
        assert_eq!(
            reject_reason(ProtoKind::Thumb, &get(), "::::"),
            "unparseable"
        );
    }

    #[test]
    fn rejects_non_localhost_authority() {
        // URL案の `asset` をauthority位置に置いた形式はhost検証で拒否
        assert_eq!(
            reject_reason(ProtoKind::Thumb, &get(), "gento-thumb://asset/1/s256"),
            "host"
        );
        assert_eq!(
            reject_reason(
                ProtoKind::Thumb,
                &get(),
                "gento-thumb://evil.example.com/asset/1/s256"
            ),
            "host"
        );
        assert_eq!(
            reject_reason(ProtoKind::Thumb, &get(), "gento-thumb://[::1]/asset/1/s256"),
            "host"
        );
        // `..` をauthorityに置いたトラバーサル形式
        assert_eq!(
            reject_reason(
                ProtoKind::Thumb,
                &get(),
                "gento-thumb://../../../etc/passwd"
            ),
            "host"
        );
    }

    #[test]
    fn rejects_port_and_userinfo() {
        assert_eq!(
            reject_reason(
                ProtoKind::Thumb,
                &get(),
                "gento-thumb://localhost:8080/asset/1/s256"
            ),
            "port"
        );
        assert_eq!(
            reject_reason(
                ProtoKind::Thumb,
                &get(),
                "gento-thumb://user:pass@localhost/asset/1/s256"
            ),
            "userinfo"
        );
    }

    #[test]
    fn rejects_fragment() {
        assert_eq!(
            reject_reason(
                ProtoKind::Thumb,
                &get(),
                "gento-thumb://localhost/asset/1/s256#x"
            ),
            "fragment"
        );
    }

    #[test]
    fn rejects_percent_encoded_separators() {
        // %2F (エンコードされたパス区切り)
        assert_eq!(
            reject_reason(
                ProtoKind::Thumb,
                &get(),
                "gento-thumb://localhost/asset/1%2F2/s256"
            ),
            "id"
        );
        // %2E%2E (エンコードされた `..`) — url crateはWHATWG準拠で
        // パーセントデコード後のドットセグメントを正規化するため
        // (/asset/%2E%2E/etc → /etc)、「先頭がassetでない」検証で拒否される
        assert_eq!(
            reject_reason(
                ProtoKind::Thumb,
                &get(),
                "gento-thumb://localhost/asset/%2E%2E/etc"
            ),
            "asset_prefix"
        );
        // %5C (エンコードされたバックスラッシュ)
        assert_eq!(
            reject_reason(
                ProtoKind::Thumb,
                &get(),
                "gento-thumb://localhost/asset/1%5C../s256"
            ),
            "id"
        );
        // 二重エンコード
        assert_eq!(
            reject_reason(
                ProtoKind::Thumb,
                &get(),
                "gento-thumb://localhost/asset/%252E%252E/etc"
            ),
            "id"
        );
        // NUL
        assert_eq!(
            reject_reason(
                ProtoKind::Thumb,
                &get(),
                "gento-thumb://localhost/asset/%00/s256"
            ),
            "id"
        );
    }

    #[test]
    fn rejects_backslash_and_dot_segments() {
        // バックスラッシュ区切り (非特殊schemeではパス区切りではないが、拒否)
        assert_eq!(
            reject_reason(
                ProtoKind::Thumb,
                &get(),
                "gento-thumb://localhost/asset/1\\..\\..\\etc/passwd"
            ),
            "id"
        );
        // パス内ドットセグメント (URLパーサが正規化後、先頭がassetでなくなる)
        assert_eq!(
            reject_reason(
                ProtoKind::Thumb,
                &get(),
                "gento-thumb://localhost/asset/../../etc/passwd"
            ),
            "asset_prefix"
        );
        assert_eq!(
            reject_reason(
                ProtoKind::Original,
                &get(),
                "gento-original://localhost/asset/..%2F..%2Fetc%2Fpasswd"
            ),
            "id"
        );
    }

    #[test]
    fn rejects_wrong_segment_shape() {
        // 過剰セグメント
        assert_eq!(
            reject_reason(
                ProtoKind::Thumb,
                &get(),
                "gento-thumb://localhost/asset/1/s256/extra"
            ),
            "segments"
        );
        // 末尾スラッシュ (空セグメント)
        assert_eq!(
            reject_reason(
                ProtoKind::Thumb,
                &get(),
                "gento-thumb://localhost/asset/1/s256/"
            ),
            "segments"
        );
        // 空セグメント
        assert_eq!(
            reject_reason(
                ProtoKind::Thumb,
                &get(),
                "gento-thumb://localhost/asset//s256"
            ),
            "id"
        );
        // size_class不足 (thumb)
        assert_eq!(
            reject_reason(ProtoKind::Thumb, &get(), "gento-thumb://localhost/asset/1"),
            "segments"
        );
        // id不足
        assert_eq!(
            reject_reason(ProtoKind::Thumb, &get(), "gento-thumb://localhost/asset"),
            "segments"
        );
        assert_eq!(
            reject_reason(
                ProtoKind::Preview,
                &get(),
                "gento-preview://localhost/asset"
            ),
            "segments"
        );
        // previewにサイズクラスは不要
        assert_eq!(
            reject_reason(
                ProtoKind::Preview,
                &get(),
                "gento-preview://localhost/asset/1/s256"
            ),
            "segments"
        );
        // 先頭セグメント不一致
        assert_eq!(
            reject_reason(
                ProtoKind::Thumb,
                &get(),
                "gento-thumb://localhost/img/1/s256"
            ),
            "asset_prefix"
        );
    }

    #[test]
    fn rejects_invalid_ids() {
        assert_eq!(
            reject_reason(
                ProtoKind::Thumb,
                &get(),
                "gento-thumb://localhost/asset/abc/s256"
            ),
            "id"
        );
        // 21桁
        assert_eq!(
            reject_reason(
                ProtoKind::Thumb,
                &get(),
                "gento-thumb://localhost/asset/100000000000000000000/s256"
            ),
            "id"
        );
        // 20桁だがu64オーバーフロー
        assert_eq!(
            reject_reason(
                ProtoKind::Thumb,
                &get(),
                "gento-thumb://localhost/asset/99999999999999999999/s256"
            ),
            "id"
        );
        // プラス符号・小数
        assert_eq!(
            reject_reason(
                ProtoKind::Thumb,
                &get(),
                "gento-thumb://localhost/asset/+1/s256"
            ),
            "id"
        );
        assert_eq!(
            reject_reason(
                ProtoKind::Thumb,
                &get(),
                "gento-thumb://localhost/asset/1.0/s256"
            ),
            "id"
        );
    }

    #[test]
    fn rejects_size_class_not_in_enum() {
        assert_eq!(
            reject_reason(
                ProtoKind::Thumb,
                &get(),
                "gento-thumb://localhost/asset/1/s1024"
            ),
            "size_class"
        );
        // thumbスキームにpreview2048は不許可 (enum完全一致)
        assert_eq!(
            reject_reason(
                ProtoKind::Thumb,
                &get(),
                "gento-thumb://localhost/asset/1/preview2048"
            ),
            "size_class"
        );
        assert_eq!(
            reject_reason(
                ProtoKind::Thumb,
                &get(),
                "gento-thumb://localhost/asset/1/S256"
            ),
            "size_class"
        );
    }

    #[test]
    fn rejects_unknown_or_invalid_query() {
        let base = "gento-thumb://localhost/asset/1/s256";
        assert_eq!(
            reject_reason(ProtoKind::Thumb, &get(), &format!("{base}?x=1")),
            "query"
        );
        assert_eq!(
            reject_reason(ProtoKind::Thumb, &get(), &format!("{base}?v=1&x=2")),
            "query"
        );
        assert_eq!(
            reject_reason(ProtoKind::Thumb, &get(), &format!("{base}?v=abc")),
            "query"
        );
        assert_eq!(
            reject_reason(ProtoKind::Thumb, &get(), &format!("{base}?v=")),
            "query"
        );
        assert_eq!(
            reject_reason(ProtoKind::Thumb, &get(), &format!("{base}?V=1")),
            "query"
        );
        // 19桁
        let long = "1".repeat(19);
        assert_eq!(
            reject_reason(ProtoKind::Thumb, &get(), &format!("{base}?v={long}")),
            "query"
        );
    }

    #[test]
    fn rejects_opaque_url() {
        // authorityのないopaque形式 (cannot-be-a-base)
        assert_eq!(
            reject_reason(ProtoKind::Thumb, &get(), "gento-thumb:asset/1/s256"),
            "opaque"
        );
    }

    // ===== 解決・応答 (tmpdirキャッシュfixture) =====

    fn cache_at(dir: &TestDir) -> Arc<ThumbnailCache> {
        Arc::new(
            ThumbnailCache::open_with_limit(
                dir.path().join("cache"),
                1 << 30,
                Arc::new(gento_image::stats::ThumbStats::new()),
            )
            .unwrap(),
        )
    }

    fn snapshot_for(id: u64, path: &Path, readable: bool) -> AssetSnapshot {
        let md = fs::metadata(path).unwrap();
        let mtime_ms = md
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        AssetSnapshot {
            id: AssetId::new(id),
            source_path: path.to_path_buf(),
            byte_size: md.len(),
            mtime_ms,
            readable,
        }
    }

    /// キャッシュ配置を計算し、そのパスを返す (データは呼び出し側が書く)。
    fn cache_path_for(
        cache: &ThumbnailCache,
        asset: &AssetSnapshot,
        size_class: SizeClass,
    ) -> PathBuf {
        let fingerprint = asset_fingerprint(&asset.source_path, asset.byte_size, asset.mtime_ms);
        let key = cache_key(&fingerprint, size_class);
        cache.root().join(&key.relative)
    }

    fn sparse_file(path: &Path, len: u64) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let f = File::create(path).unwrap();
        f.set_len(len).unwrap();
    }

    #[test]
    fn serves_cached_thumbnail_with_headers() {
        let dir = TestDir::new("hit");
        let cache = cache_at(&dir);
        let src = dir.path().join("src.png");
        fs::write(&src, b"fake-png-header").unwrap();
        let asset = snapshot_for(1, &src, true);
        let path = cache_path_for(&cache, &asset, SizeClass::S256);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"WEBPDATA!").unwrap();

        let resp = process_request(
            ProtoKind::Thumb,
            &get(),
            "gento-thumb://localhost/asset/1/s256?v=3",
            Some(&asset),
            Some(&cache),
        );
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.body(), b"WEBPDATA!");
        assert_eq!(
            resp.headers().get(http::header::CONTENT_TYPE).unwrap(),
            "image/webp"
        );
        assert_eq!(
            resp.headers().get(http::header::CONTENT_LENGTH).unwrap(),
            "9"
        );
        assert_eq!(
            resp.headers().get("X-Content-Type-Options").unwrap(),
            "nosniff"
        );
        assert_eq!(
            resp.headers().get(http::header::CACHE_CONTROL).unwrap(),
            CACHE_CONTROL_IMMUTABLE
        );
    }

    #[test]
    fn without_generation_query_cache_control_is_no_store() {
        let dir = TestDir::new("nostore");
        let cache = cache_at(&dir);
        let src = dir.path().join("src.png");
        fs::write(&src, b"x").unwrap();
        let asset = snapshot_for(1, &src, true);
        let path = cache_path_for(&cache, &asset, SizeClass::S256);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"WEBPDATA!").unwrap();

        let resp = process_request(
            ProtoKind::Thumb,
            &get(),
            "gento-thumb://localhost/asset/1/s256",
            Some(&asset),
            Some(&cache),
        );
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(http::header::CACHE_CONTROL).unwrap(),
            CACHE_CONTROL_NO_STORE
        );
    }

    #[test]
    fn serves_preview_and_original_content_types() {
        let dir = TestDir::new("types");
        let cache = cache_at(&dir);
        let src = dir.path().join("src.png");
        fs::write(&src, b"png-bytes-here").unwrap();
        let asset = snapshot_for(9, &src, true);
        let preview = cache_path_for(&cache, &asset, SizeClass::Preview2048);
        fs::create_dir_all(preview.parent().unwrap()).unwrap();
        fs::write(&preview, b"PREVIEWDATA").unwrap();

        let resp = process_request(
            ProtoKind::Preview,
            &get(),
            "gento-preview://localhost/asset/9?v=1",
            Some(&asset),
            Some(&cache),
        );
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(http::header::CONTENT_TYPE).unwrap(),
            "image/webp"
        );

        let resp = process_request(
            ProtoKind::Original,
            &get(),
            "gento-original://localhost/asset/9?v=1",
            Some(&asset),
            Some(&cache),
        );
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.body(), b"png-bytes-here");
        assert_eq!(
            resp.headers().get(http::header::CONTENT_TYPE).unwrap(),
            "image/png"
        );
    }

    #[test]
    fn ungenerated_thumbnail_is_immediate_404() {
        let dir = TestDir::new("miss");
        let cache = cache_at(&dir);
        let src = dir.path().join("src.png");
        fs::write(&src, b"x").unwrap();
        let asset = snapshot_for(1, &src, true);
        // キャッシュに何も書かない = 未生成

        let resp = process_request(
            ProtoKind::Thumb,
            &get(),
            "gento-thumb://localhost/asset/1/s256?v=1",
            Some(&asset),
            Some(&cache),
        );
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert!(resp.body().len() <= 32, "短い本文であること");
    }

    #[test]
    fn unknown_asset_is_404() {
        let dir = TestDir::new("unknown");
        let cache = cache_at(&dir);
        let resp = process_request(
            ProtoKind::Thumb,
            &get(),
            "gento-thumb://localhost/asset/999999999/s256?v=1",
            None,
            Some(&cache),
        );
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn malformed_and_wrong_method_statuses_via_process_request() {
        let dir = TestDir::new("status");
        let cache = cache_at(&dir);
        let src = dir.path().join("src.png");
        fs::write(&src, b"x").unwrap();
        let asset = snapshot_for(1, &src, true);

        let resp = process_request(
            ProtoKind::Thumb,
            &get(),
            "gento-thumb://localhost/asset/1/s256?x=1",
            Some(&asset),
            Some(&cache),
        );
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let resp = process_request(
            ProtoKind::Thumb,
            &http::Method::HEAD,
            "gento-thumb://localhost/asset/1/s256",
            Some(&asset),
            Some(&cache),
        );
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[test]
    fn original_symlink_is_404() {
        let dir = TestDir::new("symlink-src");
        let cache = cache_at(&dir);
        let real = dir.path().join("real.png");
        fs::write(&real, b"png").unwrap();
        let link = dir.path().join("link.png");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let asset = snapshot_for(1, &link, true);

        let resp = process_request(
            ProtoKind::Original,
            &get(),
            "gento-original://localhost/asset/1?v=1",
            Some(&asset),
            Some(&cache),
        );
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn cache_symlink_and_escape_are_404() {
        let dir = TestDir::new("symlink-cache");
        let cache = cache_at(&dir);
        let src = dir.path().join("src.png");
        fs::write(&src, b"x").unwrap();
        let asset = snapshot_for(1, &src, true);

        // キャッシュ位置に外部ファイルへのシンボリックリンク (ルート脱出)
        let target = dir.path().join("outside.webp");
        fs::write(&target, b"stolen").unwrap();
        let path = cache_path_for(&cache, &asset, SizeClass::S256);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();

        let resp = process_request(
            ProtoKind::Thumb,
            &get(),
            "gento-thumb://localhost/asset/1/s256?v=1",
            Some(&asset),
            Some(&cache),
        );
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // ===== TOCTOU競合シミュレーション (FG-1 / F1) =====
    // resolve_file (検証) と read_file_checked (open+read) の間でファイルシステムを
    // 差し替えることで、検証→open間の競合ウィンドウを単体テスト可能にしている。

    fn resolved_thumb_spec(dir: &TestDir, cache: &ThumbnailCache) -> (ServeSpec, PathBuf) {
        let src = dir.path().join("src.png");
        fs::write(&src, b"x").unwrap();
        let asset = snapshot_for(1, &src, true);
        let path = cache_path_for(cache, &asset, SizeClass::S256);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"WEBPDATA!").unwrap();
        let req = parse_ok(ProtoKind::Thumb, "gento-thumb://localhost/asset/1/s256?v=1");
        let spec = resolve_file(&req, &asset, cache).expect("resolve before swap");
        (spec, path)
    }

    #[test]
    fn final_component_swapped_to_symlink_after_resolve_is_rejected() {
        // 検証後に最終コンポーネントを外部ファイルへのシンボリックリンクへ
        // 差し替えても拒否されること (O_NOFOLLOW + fstatの二重防御)。
        let dir = TestDir::new("toctou-final");
        let cache = cache_at(&dir);
        let (spec, path) = resolved_thumb_spec(&dir, &cache);

        let outside = dir.path().join("outside.webp");
        fs::write(&outside, b"stolen").unwrap();
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&outside, &path).unwrap();

        match read_file_checked(&spec) {
            Err(ReadError::Gone) => {}
            Err(ReadError::Io) => panic!("symlink swap must map to 404 (Gone), not 500"),
            Ok(bytes) => panic!("symlink swap must be rejected, got {} bytes", bytes.len()),
        }
    }

    #[test]
    fn intermediate_dir_swapped_after_resolve_is_rejected() {
        // 検証後にshardディレクトリ側を差し替えると、open自体は成功しても
        // fstatの (dev, ino) が検証時と一致しないため拒否されること。
        let dir = TestDir::new("toctou-dir");
        let cache = cache_at(&dir);
        let (spec, path) = resolved_thumb_spec(&dir, &cache);

        let shard = path.parent().unwrap().to_path_buf();
        let outside_dir = dir.path().join("outside-shard");
        fs::create_dir_all(&outside_dir).unwrap();
        fs::write(outside_dir.join(path.file_name().unwrap()), b"stolen").unwrap();
        fs::rename(&shard, dir.path().join("shard-moved")).unwrap();
        std::os::unix::fs::symlink(&outside_dir, &shard).unwrap();

        match read_file_checked(&spec) {
            Err(ReadError::Gone) => {}
            Err(ReadError::Io) => panic!("dir swap must map to 404 (Gone), not 500"),
            Ok(bytes) => panic!("dir swap must be rejected, got {} bytes", bytes.len()),
        }
    }

    #[test]
    fn fd_real_path_outside_root_is_rejected() {
        // 包含再検証自体の単体検証: (dev, ino) が正しくても、open済みfdの
        // 実パスが正規ルート外を指すspecは拒否されること。
        let dir = TestDir::new("toctou-contain");
        let cache = cache_at(&dir);
        let (mut spec, _path) = resolved_thumb_spec(&dir, &cache);
        spec.canonical_root = Some(PathBuf::from("/definitely/not/the/cache/root"));
        match read_file_checked(&spec) {
            Err(ReadError::Gone) => {}
            other => panic!("outside-root spec must be rejected, got {other:?}"),
        }
    }

    #[test]
    fn read_after_successful_checks_returns_body() {
        // 競合なしの場合は従来どおり読み取れる (回帰確認)。
        let dir = TestDir::new("toctou-clean");
        let cache = cache_at(&dir);
        let (spec, _) = resolved_thumb_spec(&dir, &cache);
        assert_eq!(read_file_checked(&spec).unwrap(), b"WEBPDATA!");
    }

    #[test]
    fn unreadable_original_is_404() {
        let dir = TestDir::new("unreadable");
        let cache = cache_at(&dir);
        let src = dir.path().join("src.png");
        fs::write(&src, b"png").unwrap();
        let asset = snapshot_for(1, &src, false);

        let resp = process_request(
            ProtoKind::Original,
            &get(),
            "gento-original://localhost/asset/1?v=1",
            Some(&asset),
            Some(&cache),
        );
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn oversize_files_are_413() {
        let dir = TestDir::new("oversize");
        let cache = cache_at(&dir);
        let src = dir.path().join("src.png");
        fs::write(&src, b"x").unwrap();
        let asset = snapshot_for(1, &src, true);

        // サムネイル上限超過 (64MiB + 1)
        let path = cache_path_for(&cache, &asset, SizeClass::S256);
        sparse_file(&path, MAX_THUMB_BYTES + 1);
        let resp = process_request(
            ProtoKind::Thumb,
            &get(),
            "gento-thumb://localhost/asset/1/s256?v=1",
            Some(&asset),
            Some(&cache),
        );
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);

        // 元ファイル上限超過 (512MiB + 1)
        let big = dir.path().join("big.png");
        sparse_file(&big, MAX_ORIGINAL_BYTES + 1);
        let big_asset = snapshot_for(2, &big, true);
        let resp = process_request(
            ProtoKind::Original,
            &get(),
            "gento-original://localhost/asset/2?v=1",
            Some(&big_asset),
            Some(&cache),
        );
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[test]
    fn error_bodies_contain_no_paths() {
        let dir = TestDir::new("leak");
        let cache = cache_at(&dir);
        let src = dir.path().join("src.png");
        fs::write(&src, b"x").unwrap();
        let asset = snapshot_for(1, &src, true);

        let responses = vec![
            process_request(
                ProtoKind::Thumb,
                &get(),
                "gento-thumb://localhost/asset/1/s256?x=1",
                Some(&asset),
                Some(&cache),
            ),
            process_request(
                ProtoKind::Thumb,
                &get(),
                "gento-thumb://localhost/asset/1/s256?v=1",
                Some(&asset),
                Some(&cache),
            ),
            process_request(
                ProtoKind::Original,
                &get(),
                "gento-original://localhost/asset/999?v=1",
                None,
                Some(&cache),
            ),
        ];
        for resp in responses {
            let body = String::from_utf8(resp.body().clone()).unwrap();
            assert!(!body.contains('/'), "本文にパス区切りを含まない: {body}");
            assert!(
                !body.contains("tmp"),
                "本文にディレクトリ名を含まない: {body}"
            );
        }
    }

    #[test]
    fn proto_stats_counts_and_percentiles() {
        let stats = ProtoStats::new();
        for ms in [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0] {
            stats.record(true, 100, ms);
        }
        stats.record(false, 0, 99.0);
        let snap = stats.snapshot();
        assert_eq!(snap.requests, 11);
        assert_eq!(snap.ok, 10);
        assert_eq!(snap.bytes_sent, 1000);
        assert_eq!(snap.p50_ms, Some(6.0));
        assert!(snap.p95_ms.is_some());
        assert!(snap.p99_ms.is_some());

        let empty = ProtoStats::new().snapshot();
        assert_eq!(empty.requests, 0);
        assert_eq!(empty.p50_ms, None);
    }
}
