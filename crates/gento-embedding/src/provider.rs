//! 埋め込みプロバイダ (docs/embedding-runtime.md §11)。
//!
//! [`EmbeddingProvider`] traitの背後にONNX推論を置く。初期実装は
//! [`RuriOnnxProvider`] (ruri-v3 ONNX + CPU)。将来はOpenAI互換
//! `/v1/embeddings` 等を追加する。
//!
//! 推論手順 (§6。CONV-1検証済みの正本手順):
//! 1. manifest定義の接頭辞を付加 (query/docで切替)
//! 2. tokenizerでトークン化 (`enable_truncation(max_length=manifest値)`)
//! 3. ONNX Runtime (CPU EP) で推論: inputs `input_ids`/`attention_mask`
//!    (i64) → output `last_hidden_state`
//! 4. attention mask考慮のmean pooling
//! 5. L2正規化
//!
//! ort APIの隔離: ort (RC版) への依存はこのモジュール (とworkerバイナリ)
//! に限定する — バージョン更新影響を最小化する (dependency-notes参照)。

use crate::manifest::{model_spec, ModelSpec, MODEL_RURI_V3_30M, MODEL_RURI_V3_70M};
use ndarray::Array2;
use ort::session::{builder::GraphOptimizationLevel, Session};
use tokenizers::Tokenizer;

/// 埋め込み実行の共通インターフェース (§11)。
/// 推論は同期的 (ワーカープロセス内で実行されることを前提)。
pub trait EmbeddingProvider {
    fn model_info(&self) -> ModelInfo;
    fn embed_query(&mut self, text: &str) -> Result<Vec<f32>, ProviderError>;
    fn embed_documents(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, ProviderError>;
}

/// モデル情報 (IPC表示用)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInfo {
    pub model_id: String,
    pub package_name: String,
    pub package_version: u32,
    pub repository_id: String,
    pub revision: String,
    pub license: String,
    pub embedding_dim: usize,
    pub max_input_length: usize,
    pub distance_metric: &'static str,
}

/// プロバイダエラー。
#[derive(Debug)]
pub enum ProviderError {
    Manifest(String),
    Package(String),
    Tokenizer(String),
    Inference(String),
    Shape(String),
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for ProviderError {}

impl ProviderError {
    fn message(&self) -> String {
        match self {
            ProviderError::Manifest(m) => format!("manifestエラー: {m}"),
            ProviderError::Package(m) => format!("モデルパッケージエラー: {m}"),
            ProviderError::Tokenizer(m) => format!("tokenizerエラー: {m}"),
            ProviderError::Inference(m) => format!("推論エラー: {m}"),
            ProviderError::Shape(m) => format!("テンソル形状エラー: {m}"),
        }
    }
}

/// mean pooling + L2正規化の実行方法の指定 (内部ヘルパ)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputKind {
    Query,
    Document,
}

/// ruri-v3 ONNXプロバイダ (CPU実行)。
pub struct RuriOnnxProvider {
    spec: ModelSpec,
    tokenizer: Tokenizer,
    session: Session,
    dim: usize,
}

impl RuriOnnxProvider {
    /// モデルパッケージディレクトリ (`package_dir/model.onnx` 等) から
    /// ロードする。呼び出し側でmanifest検証 (`verify_package`) 済みの
    /// パッケージを渡すこと。
    pub fn load(
        model_id: &str,
        package_dir: &std::path::Path,
        threads: Option<u32>,
    ) -> Result<Self, ProviderError> {
        let spec = model_spec(model_id).map_err(ProviderError::Manifest)?;
        let onnx_path = package_dir.join("model.onnx");
        if !onnx_path.is_file() {
            return Err(ProviderError::Package(format!(
                "model.onnxがありません: {}",
                onnx_path.display()
            )));
        }
        // CONV-1教訓: LlamaTokenizerFast系はfrom_slow問題があるため
        // tokenizer.jsonを直接読む。切り詰めはtruncation設定しないと
        // 特殊トークン位置がズレる (enable_truncation相当)。
        let tokenizer_path = package_dir.join("tokenizer.json");
        let mut tokenizer = Tokenizer::from_file(&tokenizer_path).map_err(|e| {
            ProviderError::Tokenizer(format!(
                "{} の読み込みに失敗: {e}",
                tokenizer_path.display()
            ))
        })?;
        tokenizer
            .with_truncation(Some(tokenizers::TruncationParams {
                max_length: spec.max_input_length,
                ..Default::default()
            }))
            .map_err(|e| ProviderError::Tokenizer(format!("truncation設定失敗: {e}")))?;

        // バッチ内最長へのパディング (attention maskで除外するため
        // 埋め込み結果には影響しない)。padトークンは語彙の既定
        // (<pad> → <unk> → id 0) — mask考慮poolingで無害。
        let pad_id: u32 = tokenizer
            .token_to_id("<pad>")
            .or_else(|| tokenizer.token_to_id("<unk>"))
            .unwrap_or(0);
        let pad_token = tokenizer
            .id_to_token(pad_id)
            .unwrap_or_else(|| "<pad>".to_string());
        tokenizer.with_padding(Some(tokenizers::PaddingParams {
            strategy: tokenizers::PaddingStrategy::BatchLongest,
            direction: tokenizers::PaddingDirection::Right,
            pad_id,
            pad_token,
            ..Default::default()
        }));

        let mut builder = Session::builder()
            .map_err(|e| ProviderError::Inference(format!("session builder失敗: {e}")))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| ProviderError::Inference(format!("最適化設定失敗: {e}")))?;
        if let Some(threads) = threads {
            builder = builder
                .with_intra_threads(threads as usize)
                .map_err(|e| ProviderError::Inference(format!("スレッド数設定失敗: {e}")))?;
        }
        let session = builder
            .commit_from_file(&onnx_path)
            .map_err(|e| ProviderError::Inference(format!("モデルロード失敗: {e}")))?;

        Ok(Self {
            dim: spec.embedding_dim,
            spec,
            tokenizer,
            session,
        })
    }

    /// 既定のintra-opスレッド数 (控えめ: 論理コア数の1/4 ≒ 物理コアの半分。
    /// `available_parallelism`は論理コアを返すため、HT環境では物理≈論理/2、
    /// その半分 = 論理/4。最小1)。
    pub fn default_threads() -> u32 {
        let logical = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2);
        ((logical / 4).max(1)) as u32
    }

    fn embed_batch(
        &mut self,
        texts: &[String],
        kind: InputKind,
    ) -> Result<Vec<Vec<f32>>, ProviderError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let prefix = match kind {
            InputKind::Query => self.spec.query_prefix.clone(),
            InputKind::Document => self.spec.document_prefix.clone(),
        };
        let prefixed: Vec<String> = texts.iter().map(|t| format!("{prefix}{t}")).collect();
        // add_special_tokens=true: 参照実装 (SentenceTransformers) は
        // BOS/EOSを付けて推論する (tokenizer_configのadd_bos/eos_token=true)。
        // falseにすると埋め込み品質が大きく劣化する (評価で実証済み —
        // docs/dev/embedding-eval.md参照)。
        let encodings = self
            .tokenizer
            .encode_batch(prefixed, true)
            .map_err(|e| ProviderError::Tokenizer(format!("トークン化失敗: {e}")))?;
        let batch = encodings.len();
        let max_len = encodings
            .iter()
            .map(|e| e.get_ids().len())
            .max()
            .unwrap_or(1)
            .max(1);
        let mut ids = vec![0i64; batch * max_len];
        let mut mask = vec![0i64; batch * max_len];
        for (row, encoding) in encodings.iter().enumerate() {
            ids[row * max_len..row * max_len + encoding.get_ids().len()]
                .iter_mut()
                .zip(encoding.get_ids().iter())
                .for_each(|(slot, id)| *slot = *id as i64);
            mask[row * max_len..row * max_len + encoding.get_attention_mask().len()]
                .iter_mut()
                .zip(encoding.get_attention_mask().iter())
                .for_each(|(slot, m)| *slot = *m as i64);
        }
        let ids_array = Array2::from_shape_vec((batch, max_len), ids)
            .map_err(|e| ProviderError::Shape(format!("input_ids形状構築失敗: {e}")))?;
        let mask_array = Array2::from_shape_vec((batch, max_len), mask)
            .map_err(|e| ProviderError::Shape(format!("attention_mask形状構築失敗: {e}")))?;
        let ids_tensor = ort::value::Tensor::from_array(ids_array)
            .map_err(|e| ProviderError::Inference(format!("input_idsテンソル構築失敗: {e}")))?;
        let mask_tensor = ort::value::Tensor::from_array(mask_array.clone()).map_err(|e| {
            ProviderError::Inference(format!("attention_maskテンソル構築失敗: {e}"))
        })?;

        let outputs = self
            .session
            .run(ort::inputs![
                "input_ids" => ids_tensor,
                "attention_mask" => mask_tensor,
            ])
            .map_err(|e| ProviderError::Inference(format!("推論実行失敗: {e}")))?;
        let hidden = outputs["last_hidden_state"]
            .try_extract_array::<f32>()
            .map_err(|e| ProviderError::Inference(format!("出力テンソル抽出失敗: {e}")))?;
        let hidden_view = hidden.view();
        if hidden_view.shape() != [batch, max_len, self.dim] {
            return Err(ProviderError::Shape(format!(
                "last_hidden_stateの形状が期待と違います: {:?} (期待 [{batch}, {max_len}, {}])",
                hidden_view.shape(),
                self.dim
            )));
        }
        let mut vectors = Vec::with_capacity(batch);
        for row in 0..batch {
            let mut sum = vec![0f32; self.dim];
            let mut count = 0f32;
            for col in 0..max_len {
                let m = mask_array[[row, col]] as f32;
                if m == 0.0 {
                    continue;
                }
                count += 1.0;
                for (d, slot) in sum.iter_mut().enumerate() {
                    *slot += hidden_view[[row, col, d]];
                }
            }
            if count == 0.0 {
                return Err(ProviderError::Shape(format!(
                    "行 {row} のattention maskがすべて0です"
                )));
            }
            let vector = mean_pool_l2(&sum, count);
            vectors.push(vector);
        }
        Ok(vectors)
    }
}

impl EmbeddingProvider for RuriOnnxProvider {
    fn model_info(&self) -> ModelInfo {
        ModelInfo {
            model_id: self.spec.model_id.clone(),
            package_name: self.spec.package_name.clone(),
            package_version: self.spec.package_version,
            repository_id: self.spec.repository_id.clone(),
            revision: self.spec.revision.clone(),
            license: self.spec.license.clone(),
            embedding_dim: self.spec.embedding_dim,
            max_input_length: self.spec.max_input_length,
            distance_metric: "cosine",
        }
    }

    fn embed_query(&mut self, text: &str) -> Result<Vec<f32>, ProviderError> {
        let mut vectors = self.embed_batch(&[text.to_string()], InputKind::Query)?;
        vectors
            .pop()
            .ok_or_else(|| ProviderError::Inference("空の結果".to_string()))
    }

    fn embed_documents(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, ProviderError> {
        self.embed_batch(texts, InputKind::Document)
    }
}

/// mean pooling結果 (= Σhidden / count) をL2正規化して返す。
/// 純粋関数 (unit testで数値検証)。
pub fn mean_pool_l2(sum: &[f32], count: f32) -> Vec<f32> {
    let mut pooled: Vec<f32> = sum.iter().map(|v| v / count).collect();
    let norm: f32 = pooled.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        for v in pooled.iter_mut() {
            *v /= norm;
        }
    }
    pooled
}

/// 標準モデルIDの正当性 (worker引数検証用)。
pub fn is_known_model(model_id: &str) -> bool {
    model_id == MODEL_RURI_V3_70M || model_id == MODEL_RURI_V3_30M
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mean_pool_l2_normalizes_to_unit() {
        let sum = vec![3.0, 4.0];
        let v = mean_pool_l2(&sum, 2.0);
        // mean = [1.5, 2.0], norm = 2.5
        assert!((v[0] - 0.6).abs() < 1e-6, "{v:?}");
        assert!((v[1] - 0.8).abs() < 1e-6, "{v:?}");
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
    }

    #[test]
    fn mean_pool_l2_zero_vector_stays_zero() {
        let v = mean_pool_l2(&[0.0, 0.0, 0.0], 5.0);
        assert!(v.iter().all(|x| *x == 0.0), "ゼロベクトルは正規化しない");
    }

    #[test]
    fn mean_pool_l2_negative_values() {
        let v = mean_pool_l2(&[-2.0, 0.0, 2.0], 2.0);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
        assert!(v[0] < 0.0 && v[2] > 0.0);
    }

    #[test]
    fn known_models() {
        assert!(is_known_model("ruri-v3-70m"));
        assert!(is_known_model("ruri-v3-30m"));
        assert!(!is_known_model("ruri-v3-130m"));
        assert!(!is_known_model(""));
    }

    #[test]
    fn load_missing_package_errors_cleanly() {
        let dir = std::env::temp_dir().join(format!(
            "gento-embed-provider-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let result = RuriOnnxProvider::load("ruri-v3-30m", &dir, None);
        assert!(matches!(result, Err(ProviderError::Package(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_unknown_model_errors_cleanly() {
        let result = RuriOnnxProvider::load("not-a-model", std::path::Path::new("/tmp"), None);
        assert!(matches!(result, Err(ProviderError::Manifest(_))));
    }

    #[test]
    fn error_messages_are_japanese_prefixed() {
        assert!(ProviderError::Manifest("x".into())
            .to_string()
            .contains("manifest"));
        assert!(ProviderError::Package("x".into())
            .to_string()
            .contains("パッケージ"));
    }
}
