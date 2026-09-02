//! ワーカープロトコル (stdio長さ前置JSON。docs/embedding-runtime.md §5)。
//!
//! ホスト (gento-tauri) と独立ワーカープロセス (`gento-embedding-worker`)
//! の間の通信契約。両者が同じlibの型を使うため、形状の drift は
//! コンパイル時に防がれる。
//!
//! フレーム形式: 4バイトlittle-endian u32長さ + UTF-8 JSON本体。
//! 画像データは流さない (設計§5)。テキスト・実行設定・状態・ベクトルのみ。

use serde::{Deserialize, Serialize};

/// 1フレームの最大バイト数 (ベクトルバッチを想定した安全域。
/// 8192トークン×384次元のリクエストよりも十分大きい)。
pub const MAX_FRAME_BYTES: u32 = 64 * 1024 * 1024;

/// ホスト → ワーカー要求。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum WorkerRequest {
    /// モデルをロードする (`models_dir/<model_id>` を検証してから)。
    Load {
        id: u64,
        model: String,
        /// モデルパッケージのルートディレクトリ。
        models_dir: String,
        /// ort intra-opスレッド数 (None = 既定: 控えめ自動)。
        threads: Option<u32>,
    },
    /// モデルをアンロードする (未ロードならno-op)。
    Unload { id: u64 },
    /// 検索クエリ1件を埋め込む (クエリ接頭辞)。
    EmbedQuery { id: u64, text: String },
    /// 検索文書バッチを埋め込む (文書接頭辞)。
    EmbedDocuments { id: u64, texts: Vec<String> },
    /// 状態を返す。
    Status { id: u64 },
    /// Graceful終了。
    Shutdown { id: u64 },
}

/// ワーカー → ホスト応答。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerResponse {
    pub id: u64,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<WorkerResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 応答本文 (cmd別)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkerResult {
    Load { model_info: ModelInfo },
    Unload,
    EmbedQuery { vector: Vec<f32> },
    EmbedDocuments { vectors: Vec<Vec<f32>> },
    Status { status: WorkerStatus },
    Shutdown,
}

/// モデル情報 (§11 EmbeddingProvider::model_info)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub model_id: String,
    pub package_name: String,
    pub package_version: u32,
    pub repository_id: String,
    pub revision: String,
    pub license: String,
    pub embedding_dim: usize,
    pub max_input_length: usize,
    pub distance_metric: String,
}

/// ワーカー状態 (`status` 応答)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerStatus {
    /// ロード済みモデルID (未ロード = None)。
    pub loaded_model: Option<String>,
    /// ロードしてからの経過ms。
    pub loaded_for_ms: u64,
    /// 最後の推論からの経過ms (未使用 = None)。
    pub idle_for_ms: Option<u64>,
    /// 処理したクエリ累計。
    pub queries_processed: u64,
    /// 処理した文書累計。
    pub documents_processed: u64,
}

impl WorkerResponse {
    pub fn ok(id: u64, result: WorkerResult) -> Self {
        Self {
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: u64, message: impl Into<String>) -> Self {
        Self {
            id,
            ok: false,
            result: None,
            error: Some(message.into()),
        }
    }
}

/// 1フレームを書き出す (長さ前置 + JSON)。
pub fn write_frame(writer: &mut impl std::io::Write, value: &impl Serialize) -> Result<(), String> {
    let mut json = serde_json::to_vec(value).map_err(|e| format!("シリアライズ失敗: {e}"))?;
    if json.len() > MAX_FRAME_BYTES as usize {
        return Err(format!(
            "フレームが大きすぎます: {} bytes (上限 {MAX_FRAME_BYTES})",
            json.len()
        ));
    }
    let len = (json.len() as u32).to_le_bytes();
    writer
        .write_all(&len)
        .and_then(|_| writer.write_all(&json))
        .and_then(|_| writer.flush())
        .map_err(|e| format!("フレーム書き込み失敗: {e}"))?;
    json.clear();
    Ok(())
}

/// 1フレームを読み込む (長さ前置 + JSON)。EOFは `Ok(None)`。
pub fn read_frame<T: for<'de> Deserialize<'de>>(
    reader: &mut impl std::io::Read,
) -> Result<Option<T>, String> {
    let mut len_buf = [0u8; 4];
    match reader.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(format!("フレーム長読み取り失敗: {e}")),
    }
    let len = u32::from_le_bytes(len_buf);
    if len == 0 {
        return Err("フレーム長が0です".to_string());
    }
    if len > MAX_FRAME_BYTES {
        return Err(format!(
            "フレーム長が上限を超えています: {len} > {MAX_FRAME_BYTES}"
        ));
    }
    let mut body = vec![0u8; len as usize];
    reader
        .read_exact(&mut body)
        .map_err(|e| format!("フレーム本体読み取り失敗: {e}"))?;
    serde_json::from_slice(&body)
        .map_err(|e| format!("フレームJSON解析失敗: {e}"))
        .map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_round_trip(request: &WorkerRequest) -> WorkerRequest {
        let mut buf = Vec::new();
        write_frame(&mut buf, request).expect("write");
        let mut cursor = std::io::Cursor::new(buf);
        read_frame(&mut cursor)
            .expect("read")
            .expect("フレームがある")
    }

    #[test]
    fn request_frame_round_trip_all_commands() {
        let requests = vec![
            WorkerRequest::Load {
                id: 1,
                model: "ruri-v3-70m".to_string(),
                models_dir: "/models".to_string(),
                threads: Some(2),
            },
            WorkerRequest::Unload { id: 2 },
            WorkerRequest::EmbedQuery {
                id: 3,
                text: "検索クエリ: 猫の写真".to_string(),
            },
            WorkerRequest::EmbedDocuments {
                id: 4,
                texts: vec!["文書1".to_string(), "文書2".to_string()],
            },
            WorkerRequest::Status { id: 5 },
            WorkerRequest::Shutdown { id: 6 },
        ];
        for request in &requests {
            assert_eq!(&frame_round_trip(request), request);
        }
    }

    #[test]
    fn response_json_shape_has_id_ok_result() {
        let response = WorkerResponse::ok(
            42,
            WorkerResult::EmbedQuery {
                vector: vec![0.5, -0.5],
            },
        );
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["id"], 42);
        assert_eq!(json["ok"], true);
        assert_eq!(json["result"]["kind"], "embed_query");
        assert_eq!(json["result"]["vector"], serde_json::json!([0.5, -0.5]));
        assert!(json.get("error").is_none(), "ok時はerrorフィールドを省略");
        let error = WorkerResponse::error(43, "ロード失敗");
        let json = serde_json::to_value(&error).unwrap();
        assert_eq!(json["ok"], false);
        assert_eq!(json["error"], "ロード失敗");
        assert!(json.get("result").is_none());
    }

    #[test]
    fn request_json_uses_cmd_tag() {
        let json = serde_json::to_value(WorkerRequest::Shutdown { id: 9 }).unwrap();
        assert_eq!(json["cmd"], "shutdown");
        assert_eq!(json["id"], 9);
        let json = serde_json::to_value(WorkerRequest::EmbedDocuments {
            id: 10,
            texts: vec!["a".to_string()],
        })
        .unwrap();
        assert_eq!(json["cmd"], "embed_documents");
    }

    #[test]
    fn empty_stream_returns_none() {
        let mut cursor = std::io::Cursor::new(Vec::<u8>::new());
        let frame: Option<WorkerRequest> = read_frame(&mut cursor).expect("read");
        assert!(frame.is_none());
    }

    #[test]
    fn zero_length_frame_is_rejected() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&0u32.to_le_bytes());
        let mut cursor = std::io::Cursor::new(buf);
        let result: Result<Option<WorkerRequest>, String> = read_frame(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn oversized_frame_length_is_rejected() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(MAX_FRAME_BYTES + 1).to_le_bytes());
        let mut cursor = std::io::Cursor::new(buf);
        let result: Result<Option<WorkerRequest>, String> = read_frame(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn invalid_json_body_is_rejected() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&5u32.to_le_bytes());
        buf.extend_from_slice(b"!!!!!");
        let mut cursor = std::io::Cursor::new(buf);
        let result: Result<Option<WorkerRequest>, String> = read_frame(&mut cursor);
        assert!(result.is_err());
    }
}
