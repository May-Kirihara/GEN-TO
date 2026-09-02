//! GEN-TO AI統合crate (MVP WP-10)。
//!
//! OpenAI互換endpoint (llama.cpp等) への接続・versionedプロンプト・
//! APIキー保管・AI実行パイプラインを担う。永続ジョブキューの
//! 状態管理は gento-db (jobs表) が正本で、本crateは実行側。
//!
//! 依存方向: ai → db → core (tauri → ai)。HTTPはreqwest blocking
//! (workerはstd::thread。dependency-notes.md参照)。

pub mod apikey;
pub mod client;
pub mod imaging;
pub mod pipeline;
pub mod prompt;
#[cfg(any(debug_assertions, feature = "devtools"))]
pub mod testserver;

pub use apikey::{ApiKey, SecretStore, SecretStoreKind, StoredSecretRef};
pub use client::{AiClient, AiError, ChatMessage, ChatResponse, TestReport};
pub use pipeline::{run_caption_tags_job, AiRunOutcome, CaptionTagsResult, PipelineOptions};
pub use prompt::{
    build_caption_tags_messages, parse_caption_tags_response, AiMetadataHints, PROMPT_VERSION,
};
