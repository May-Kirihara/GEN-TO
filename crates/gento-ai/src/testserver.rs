//! テスト・検証用のOpenAI互換mock endpoint (WP-10)。
//!
//! stdのみでHTTP/1.1を最小実装したin-processサーバー:
//! - `GET /v1/models` → モデル一覧JSON
//! - `POST /v1/chat/completions` → 固定/注入応答。Bearer検証・遅延・
//!   ステータス注入 (500等)・structured応答 (response_format検出)・
//!   巨大ボディ (Content-Length/chunked)・リダイレクト注入が可能
//!
//! cargo test・debug build・devtools featureビルドでのみコンパイルされる
//! (release通常ビルドからは除去 — WP-0のdevtools境界規約と同一)。
//! E2E (gento-tauri ai.rs tests) がこのサーバーを立てて
//! 「llama.cpp互換endpointへの接続」を自動証明する。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// 巨大ボディ応答の形式 (応答上限試験: FG-WP10 blocker 4)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HugeBodyMode {
    /// Content-Length付き (検査はヘッダ段階で拒絶できる形)。
    ContentLength,
    /// Transfer-Encoding: chunked (Content-Length無し — ストリーミング
    /// 読み取り上限の検証用)。
    Chunked,
}

/// mockの動作注入。
#[derive(Debug, Clone)]
pub struct MockBehavior {
    /// この値を期待するBearer (検証失敗は401)。
    pub expected_bearer: Option<String>,
    /// chat/completionsの固定応答本文 (contentに入る文字列)。
    pub chat_content: String,
    /// chat応答のHTTPステータス (遅延・障害注入)。
    pub chat_status: u16,
    /// 応答前の遅延 (timeout試験)。
    pub delay: Duration,
    /// request_format: json_object を含むリクエストへの応答を
    /// JSON文字列に切り替える (test_structured模倣)。
    pub honor_response_format: bool,
    /// 設定時: chat/completionsの代わりに3 MiBの巨大ボディを返す
    /// (応答読み取り上限の試験)。
    pub huge_body: Option<HugeBodyMode>,
    /// 設定時: chat/completionsは302 + Locationで応答 (リダイレクト
    /// 不追従の試験。Locationは絶対URLを指定すること)。
    pub redirect_to: Option<String>,
}

impl Default for MockBehavior {
    fn default() -> Self {
        MockBehavior {
            expected_bearer: None,
            chat_content: r#"{"tags":["青空","雲"],"caption":"青い空に白い雲が広がる画像"}"#
                .to_string(),
            chat_status: 200,
            delay: Duration::from_millis(0),
            honor_response_format: true,
            huge_body: None,
            redirect_to: None,
        }
    }
}

/// 受け取ったリクエストの記録 (検証用)。
#[derive(Debug, Default)]
pub struct RequestLog {
    pub chat_calls: AtomicU64,
    pub models_calls: AtomicU64,
    pub unauthorized: AtomicU64,
    /// 最後のchatリクエスト本文 (GPS/EXIF混入検証に使う)。
    pub last_body: std::sync::Mutex<String>,
    /// 処理中リクエスト数 (同時実行検出)。
    in_flight: AtomicI64,
    /// 同時処理数の最大観測値 (concurrency制限の実証に使う)。
    pub concurrent_max: AtomicU64,
}

/// 起動済みmockサーバー (dropで停止)。
pub struct MockServer {
    pub port: u16,
    pub base_url: String,
    behavior: Arc<std::sync::Mutex<MockBehavior>>,
    pub log: Arc<RequestLog>,
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl MockServer {
    /// `127.0.0.1:0` で起動し、base_url (`http://127.0.0.1:{port}/v1`) を返す。
    pub fn start(behavior: MockBehavior) -> Self {
        Self::start_on_ephemeral(behavior)
    }

    /// 指定portで起動 (endpoint復帰試験: 一度閉じたportで再/listen)。
    /// portが奪われていた場合はpanic (テスト固有の前提)。
    pub fn start_on_port(port: u16, behavior: MockBehavior) -> Self {
        Self::start_impl(
            TcpListener::bind(("127.0.0.1", port)).expect("bind specific port"),
            behavior,
        )
    }

    fn start_on_ephemeral(behavior: MockBehavior) -> Self {
        Self::start_impl(
            TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port"),
            behavior,
        )
    }

    fn start_impl(listener: TcpListener, behavior: MockBehavior) -> Self {
        let port = listener.local_addr().unwrap().port();
        let behavior = Arc::new(std::sync::Mutex::new(behavior));
        let log = Arc::new(RequestLog::default());
        let stop = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::new();
        {
            let behavior = Arc::clone(&behavior);
            let log = Arc::clone(&log);
            let stop = Arc::clone(&stop);
            threads.push(
                std::thread::Builder::new()
                    .name("mock-openai".into())
                    .spawn(move || {
                        for stream in listener.incoming() {
                            if stop.load(Ordering::SeqCst) {
                                break;
                            }
                            let Ok(stream) = stream else { continue };
                            let behavior = Arc::clone(&behavior);
                            let log = Arc::clone(&log);
                            // 接続毎にスレッド (同時実行テストのため)
                            std::thread::spawn(move || {
                                let _ = handle(stream, &behavior, &log);
                            });
                        }
                    })
                    .expect("spawn mock server"),
            );
        }
        MockServer {
            port,
            base_url: format!("http://127.0.0.1:{port}/v1"),
            behavior,
            log,
            stop,
            threads,
        }
    }

    /// 動作を差し替える (例: 500注入 → 200へ戻す)。
    pub fn set_behavior(&self, behavior: MockBehavior) {
        *self.behavior.lock().unwrap() = behavior;
    }

    pub fn chat_calls(&self) -> u64 {
        self.log.chat_calls.load(Ordering::SeqCst)
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // ループはacceptブロック中のため、自分へ接続して起こす
        if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", self.port)) {
            let _ = stream.write_all(
                b"GET /v1/models HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
            );
            let _ = stream.flush();
        }
        for handle in self.threads.drain(..) {
            let _ = handle.join();
        }
    }
}

fn handle(
    mut stream: TcpStream,
    behavior: &Arc<std::sync::Mutex<MockBehavior>>,
    log: &RequestLog,
) -> std::io::Result<()> {
    // 同時処理数の追跡 (concurrency制限の実証 — 遅延を含む全処理時間)
    let now_in_flight = log.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
    let mut observed_max = log.concurrent_max.load(Ordering::SeqCst) as i64;
    while now_in_flight > observed_max {
        match log.concurrent_max.compare_exchange(
            observed_max as u64,
            now_in_flight as u64,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) => break,
            Err(actual) => observed_max = actual as i64,
        }
    }
    let result = handle_inner(&mut stream, behavior, log);
    log.in_flight.fetch_sub(1, Ordering::SeqCst);
    result
}

fn handle_inner(
    stream: &mut TcpStream,
    behavior: &Arc<std::sync::Mutex<MockBehavior>>,
    log: &RequestLog,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    // acceptループの停止判定は接続内では行わない (単発リクエストのため)
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();

    // ヘッダ
    let mut content_length = 0usize;
    let mut authorization: Option<String> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim();
            match name.as_str() {
                "content-length" => {
                    content_length = value.parse().unwrap_or(0);
                }
                "authorization" => {
                    authorization = Some(value.to_string());
                }
                _ => {}
            }
        }
    }
    // 本体
    let mut body = vec![0u8; content_length.min(64 * 1024 * 1024)];
    if content_length > 0 {
        reader.read_exact(&mut body)?;
    }
    let body = String::from_utf8_lossy(&body).into_owned();

    let behavior = behavior.lock().unwrap().clone();
    if !behavior.delay.is_zero() {
        std::thread::sleep(behavior.delay);
    }

    // Bearer検証 (鍵がヘッダで送られることの確認)
    if let Some(expected) = &behavior.expected_bearer {
        let ok = authorization
            .as_deref()
            .is_some_and(|value| value == format!("Bearer {expected}"));
        if !ok {
            log.unauthorized.fetch_add(1, Ordering::SeqCst);
            return respond(stream, 401, r#"{"error":{"message":"invalid api key"}}"#);
        }
    }

    match (method.as_str(), path.as_str()) {
        ("GET", "/v1/models") => {
            log.models_calls.fetch_add(1, Ordering::SeqCst);
            let payload = r#"{"object":"list","data":[{"id":"mock-vision-model"}]}"#;
            respond(stream, 200, payload)
        }
        ("POST", "/v1/chat/completions") => {
            log.chat_calls.fetch_add(1, Ordering::SeqCst);
            *log.last_body.lock().unwrap() = body.clone();
            if let Some(location) = &behavior.redirect_to {
                return respond_redirect(stream, location);
            }
            if let Some(mode) = behavior.huge_body {
                return respond_huge(stream, mode);
            }
            if behavior.chat_status != 200 {
                return respond(
                    stream,
                    behavior.chat_status,
                    r#"{"error":{"message":"injected failure"}}"#,
                );
            }
            // response_formatが要求されていたら純JSONを返す模倣。
            // honor_response_format=falseは「JSON mode非対応」の模倣
            // (プロンプトへの寛容パースが効くことの検証用)。
            let wants_json = body.contains("json_object");
            let content = if wants_json && !behavior.honor_response_format {
                format!("以下が結果です:\n{}", behavior.chat_content)
            } else {
                behavior.chat_content.clone()
            };
            let payload = format!(
                r#"{{"id":"chatcmpl-mock","object":"chat.completion","created":0,"model":"mock-vision-model","choices":[{{"index":0,"message":{{"role":"assistant","content":{}}},"finish_reason":"stop"}}],"usage":{{"prompt_tokens":10,"completion_tokens":20}}"#,
                serde_json::to_string(&content).unwrap_or_default()
            );
            let payload = format!("{payload}}}");
            respond(stream, 200, &payload)
        }
        _ => respond(stream, 404, r#"{"error":{"message":"not found"}}"#),
    }
}

fn respond(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        401 => "Unauthorized",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Status",
    };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    stream.flush()
}

/// 302 + Location (リダイレクト不追従の試験用)。
fn respond_redirect(stream: &mut TcpStream, location: &str) -> std::io::Result<()> {
    let header = format!(
        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(header.as_bytes())?;
    stream.flush()
}

/// 3 MiBの巨大ボディ (応答読み取り上限の試験用)。
const HUGE_BODY_BYTES: usize = 3 * 1024 * 1024;

fn respond_huge(stream: &mut TcpStream, mode: HugeBodyMode) -> std::io::Result<()> {
    match mode {
        HugeBodyMode::ContentLength => {
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {HUGE_BODY_BYTES}\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(header.as_bytes())?;
            let chunk = [b'x'; 64 * 1024];
            let mut remaining = HUGE_BODY_BYTES;
            while remaining > 0 {
                let n = remaining.min(chunk.len());
                stream.write_all(&chunk[..n])?;
                remaining -= n;
            }
        }
        HugeBodyMode::Chunked => {
            // Transfer-Encoding: chunked — Content-Lengthを示さない
            // (ストリーミング読み取り上限の検証用)
            let header = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
            stream.write_all(header.as_bytes())?;
            let chunk = [b'x'; 64 * 1024];
            let mut remaining = HUGE_BODY_BYTES;
            while remaining > 0 {
                let n = remaining.min(chunk.len());
                stream.write_all(format!("{n:x}\r\n").as_bytes())?;
                stream.write_all(&chunk[..n])?;
                stream.write_all(b"\r\n")?;
                remaining -= n;
            }
            stream.write_all(b"0\r\n\r\n")?;
        }
    }
    stream.flush()
}
