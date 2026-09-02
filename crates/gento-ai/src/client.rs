//! OpenAI互換endpointへのHTTPクライアント (MVP WP-10。spec §11.1)。
//!
//! - `POST {base_url}/chat/completions` (messages形式。画像は
//!   `image_url` のbase64 data URL) と `GET {base_url}/models` のみを
//!   実装する (llama.cpp / OpenAI互換実装の共通部分)。
//! - `response_format: {"type":"json_object"}` は対応が実装間で分かれる
//!   ため、**既定では送らない** (テスト機能 `test_structured` で対応を
//!   確認する。プロファイル追加設定 `send_response_format` で明示ON)。
//! - エラー種別は接続失敗 (再試行で回復し得る) とAPIエラー (応答あり。
//!   ステータス+本文抜粋) を区別する。**APIキーをエラー文字列に含む
//!   経路はない** (Authorizationヘッダの値を整形しないため。
//! - 応答境界: ボディは2 MiB上限 (超過は [`AiError::Oversized`]。
//!   Content-Length検査+ストリーミング読み取りの二段構え)。リダイレクト
//!   は不追従 (3xxはそのままAPIエラー — 外向き送信の意図しない転送防止)。

use std::io::Read;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// 応答ボディの読み取り上限 (2 MiB)。超過は即エラー — 不正・悪意のある
/// endpointによる無制限送出でファイラーのメモリを枯渇させない
/// (Content-Length検査 + ストリーミング読み取り上限の二段構え)。
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

/// 接続設定 (ai_profiles行の実行時形)。
#[derive(Debug, Clone)]
pub struct AiEndpoint {
    pub base_url: String,
    pub model: String,
    pub timeout: Duration,
}

/// chat/completionsのメッセージ要素。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum ChatMessage {
    System { content: String },
    User { content: Value },
}

/// image_url要素 (OpenAI形式)。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImageUrl {
    pub url: String,
}

pub fn image_url_message(data_url: String, text: &str) -> ChatMessage {
    ChatMessage::User {
        content: json!([
            {"type": "image_url", "image_url": {"url": data_url}},
            {"type": "text", "text": text}
        ]),
    }
}

pub fn text_message(text: &str) -> ChatMessage {
    ChatMessage::User {
        content: json!(text),
    }
}

/// エラー (APIキーは含まない)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiError {
    /// 接続層の失敗 (DNS・TCP・タイムアウト。再試行で回復し得る)。
    Connect(String),
    /// HTTP応答は返ったが失敗ステータス (4xx/5xx/3xx — リダイレクト不追従)。
    Api { status: u16, body_excerpt: String },
    /// 成功応答だが内容が期待形でない (choices無し・空本文等)。
    Parse(String),
    /// 応答ボディが上限を超えた (faulty/malicious endpoint。
    /// 再試行しても同じ結果になる可能性が高いため非リトライ)。
    Oversized { status: u16, limit_bytes: usize },
}

impl std::fmt::Display for AiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AiError::Connect(msg) => write!(f, "connection failed: {msg}"),
            AiError::Api {
                status,
                body_excerpt,
            } => {
                write!(f, "endpoint returned {status}: {body_excerpt}")
            }
            AiError::Parse(msg) => write!(f, "unexpected response shape: {msg}"),
            AiError::Oversized {
                status,
                limit_bytes,
            } => write!(
                f,
                "endpoint response exceeded {limit_bytes} bytes (status {status})"
            ),
        }
    }
}

impl std::error::Error for AiError {}

impl AiError {
    /// 再試行で回復し得る失敗か (ジョブのbackoff判定)。
    /// 接続失敗と5xx系は回復見込みあり。4xx (認証・不正リクエスト)・
    /// パース失敗・応答過大は繰り返しても同じ結果のため再試行しない。
    pub fn is_retryable(&self) -> bool {
        match self {
            AiError::Connect(_) => true,
            AiError::Api { status, .. } => *status >= 500,
            AiError::Parse(_) => false,
            AiError::Oversized { .. } => false,
        }
    }
}

/// chat/completions応答の必要部分。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatResponse {
    /// choices[0].message.content。
    pub content: String,
    /// usage (あれば)。
    pub usage: Option<Value>,
}

/// OpenAI互換クライアント (reqwest blocking。workerスレッド専用)。
pub struct AiClient {
    endpoint: AiEndpoint,
    api_key: Option<crate::ApiKey>,
    client: reqwest::blocking::Client,
    send_response_format: bool,
}

impl AiClient {
    pub fn new(
        endpoint: AiEndpoint,
        api_key: Option<crate::ApiKey>,
        send_response_format: bool,
    ) -> Self {
        let timeout = endpoint.timeout;
        // フォールバックを置かない: Client::new()は失敗時panicのため従来の
        // 代替経路は実際には機能せず、かつredirect=noneの保証を破る。
        let client = reqwest::blocking::Client::builder()
            .timeout(timeout)
            // 外向き送信の意図しない転送防止: リダイレクトは明示的に不追従
            // (3xx応答はそのままApiエラーとして扱う)。
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("blocking reqwest client (timeout + redirect=none)");
        AiClient {
            endpoint,
            api_key,
            client,
            send_response_format,
        }
    }

    fn request_builder(
        &self,
        method: reqwest::Method,
        path: &str,
    ) -> reqwest::blocking::RequestBuilder {
        // base_url末尾のスラッシュ・ "/v1" 重複を許容する結合。
        let base = self.endpoint.base_url.trim_end_matches('/');
        let url = format!("{base}{path}");
        let mut builder = self.client.request(method, &url);
        if let Some(key) = &self.api_key {
            // 鍵はここでのみヘッダへ出る (エラー文字列化なし)。
            builder = builder.bearer_auth(key.secret());
        }
        builder
    }

    fn execute(
        &self,
        builder: reqwest::blocking::RequestBuilder,
    ) -> Result<reqwest::blocking::Response, AiError> {
        builder.send().map_err(|e| {
            // reqwestのエラー表示に鍵が含まれる経路はない (URLと原因のみ)
            // が、確実を期すため種別名+メッセージのみを伝播する。
            let msg = e.to_string();
            AiError::Connect(msg)
        })
    }

    /// 応答ボディを上限付きで読み取る (Content-Length検査 + ストリーミング
    /// 読み取り上限 — chunked応答も上限に到達した時点でエラー)。
    fn read_body_checked(&self, response: reqwest::blocking::Response) -> Result<String, AiError> {
        let status = response.status().as_u16();
        if let Some(length) = response.content_length() {
            if length as usize > MAX_RESPONSE_BYTES {
                return Err(AiError::Oversized {
                    status,
                    limit_bytes: MAX_RESPONSE_BYTES,
                });
            }
        }
        let mut body: Vec<u8> = Vec::new();
        let mut limited = response.take((MAX_RESPONSE_BYTES + 1) as u64);
        limited
            .read_to_end(&mut body)
            .map_err(|e| AiError::Connect(e.to_string()))?;
        if body.len() > MAX_RESPONSE_BYTES {
            return Err(AiError::Oversized {
                status,
                limit_bytes: MAX_RESPONSE_BYTES,
            });
        }
        let body = String::from_utf8_lossy(&body).into_owned();
        if !(200..300).contains(&status) {
            let excerpt: String = body.chars().take(300).collect();
            return Err(AiError::Api {
                status,
                body_excerpt: excerpt,
            });
        }
        Ok(body)
    }

    /// `GET /models` (llama.cpp/OpenAI共通)。応答本文を返す。
    pub fn list_models(&self) -> Result<String, AiError> {
        let builder = self.request_builder(reqwest::Method::GET, "/models");
        let response = self.execute(builder)?;
        self.read_body_checked(response)
    }

    /// `POST /chat/completions`。`messages` と (指定時) `response_format`
    /// を送り、最初のchoiceのcontentを返す。
    pub fn chat(
        &self,
        messages: &[ChatMessage],
        force_response_format: Option<bool>,
    ) -> Result<ChatResponse, AiError> {
        let mut body = json!({
            "model": self.endpoint.model,
            "messages": messages,
        });
        let want_json = force_response_format.unwrap_or(self.send_response_format);
        if want_json {
            body["response_format"] = json!({"type": "json_object"});
        }
        let builder = self
            .request_builder(reqwest::Method::POST, "/chat/completions")
            .json(&body);
        let response = self.execute(builder)?;
        let text = self.read_body_checked(response)?;
        let value: Value =
            serde_json::from_str(&text).map_err(|e| AiError::Parse(format!("not JSON: {e}")))?;
        let content = value
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .ok_or_else(|| AiError::Parse("missing choices[0].message.content".to_string()))?
            .to_string();
        Ok(ChatResponse {
            content,
            usage: value.get("usage").cloned(),
        })
    }
}

// ===== テスト機能 (spec §11.1: 接続・テキスト・Vision・構造化出力) =====

/// テスト1件の結果。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TestReport {
    pub ok: bool,
    /// 応答概要 (応答抜粋・ステータス。長文は切り詰め)。
    pub detail: String,
    pub elapsed_ms: u64,
    pub status: Option<u16>,
}

impl TestReport {
    fn fail(err: &AiError, started: Instant) -> Self {
        TestReport {
            ok: false,
            detail: err.to_string(),
            elapsed_ms: started.elapsed().as_millis() as u64,
            status: match err {
                AiError::Api { status, .. } => Some(*status),
                _ => None,
            },
        }
    }
}

/// 接続テスト: `/models` がHTTP応答を返すか。
pub fn test_connection(client: &AiClient) -> TestReport {
    let started = Instant::now();
    match client.list_models() {
        Ok(body) => {
            let excerpt: String = body.chars().take(200).collect();
            TestReport {
                ok: true,
                detail: format!("接続OK (models応答: {excerpt})"),
                elapsed_ms: started.elapsed().as_millis() as u64,
                status: Some(200),
            }
        }
        Err(err) => TestReport::fail(&err, started),
    }
}

/// テキスト応答テスト: 短い指示でcontentが返るか。
pub fn test_text(client: &AiClient) -> TestReport {
    let started = Instant::now();
    let messages = [
        ChatMessage::System {
            content: "あなたはテスト用の応答者です。".to_string(),
        },
        text_message("「OK」とだけ応答してください。"),
    ];
    match client.chat(&messages, Some(false)) {
        Ok(response) => {
            let excerpt: String = response.content.chars().take(200).collect();
            TestReport {
                ok: true,
                detail: format!("応答: {excerpt}"),
                elapsed_ms: started.elapsed().as_millis() as u64,
                status: Some(200),
            }
        }
        Err(err) => TestReport::fail(&err, started),
    }
}

/// Visionテスト: 1x1 PNG画像を送って応答が返るか。
pub fn test_vision(client: &AiClient) -> TestReport {
    let started = Instant::now();
    let data_url = format!("data:image/png;base64,{}", crate::imaging::TINY_PNG_BASE64);
    let messages = [
        ChatMessage::System {
            content: "画像の内容を簡潔に応答するテスト用アシスタントです。".to_string(),
        },
        image_url_message(data_url, "この画像に見えるものを短く答えてください。"),
    ];
    match client.chat(&messages, Some(false)) {
        Ok(response) => {
            let excerpt: String = response.content.chars().take(200).collect();
            TestReport {
                ok: true,
                detail: format!("応答: {excerpt}"),
                elapsed_ms: started.elapsed().as_millis() as u64,
                status: Some(200),
            }
        }
        Err(err) => TestReport::fail(&err, started),
    }
}

/// 構造化出力テスト: `response_format: json_object` でJSONが返るか。
pub fn test_structured(client: &AiClient) -> TestReport {
    let started = Instant::now();
    let messages = [
        ChatMessage::System {
            content: "必ずJSONオブジェクトのみを出力してください。".to_string(),
        },
        text_message("フィールドokにtrueを入れたJSONを返してください。"),
    ];
    match client.chat(&messages, Some(true)) {
        Ok(response) => match serde_json::from_str::<Value>(&response.content) {
            Ok(value) if value.is_object() => TestReport {
                ok: true,
                detail: format!("構造化JSON検証OK: {}", {
                    let excerpt: String = response.content.chars().take(200).collect();
                    excerpt
                }),
                elapsed_ms: started.elapsed().as_millis() as u64,
                status: Some(200),
            },
            Ok(other) => TestReport {
                ok: false,
                detail: format!("JSONオブジェクトではありません: {other:?}"),
                elapsed_ms: started.elapsed().as_millis() as u64,
                status: Some(200),
            },
            Err(err) => TestReport {
                ok: false,
                detail: format!("応答はJSONとして解釈できません ({err}): {}", {
                    let excerpt: String = response.content.chars().take(200).collect();
                    excerpt
                }),
                elapsed_ms: started.elapsed().as_millis() as u64,
                status: Some(200),
            },
        },
        Err(err) => TestReport::fail(&err, started),
    }
}
