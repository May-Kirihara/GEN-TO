//! GEN-TO 埋め込み実行基盤 (MVP WP-11)。
//!
//! # 構成
//!
//! - `manifest`: ruri-v3 ONNXパッケージの埋め込みmanifest (正本 =
//!   docs/dev/ruri3-onnx-manifest.json) とsha256検証
//! - `model_store`: モデル配置ディレクトリの解決・検証状態・削除
//! - `provider`: [`EmbeddingProvider`] trait + [`RuriOnnxProvider`]
//!   (tokenizers → ort CPU → mean pooling → L2)
//! - `worker_proto`: 独立ワーカー (`gento-embedding-worker` bin) との
//!   stdio長さ前置JSONプロトコル
//! - `download`: HF配布repoからのモデルDL (進捗・キャンセル・再開)
//! - `vector_index`: USearch HNSW索引ラッパ (WP-12。モデルidentity分離)
//!
//! # 依存方向
//!
//! gento-core ← gento-embedding ← gento-db (build_search_doc_input) /
//! gento-tauri。このcrateはDBに依存しない (ワーカープロセスを軽量に
//! 保つ — 設計§5)。

pub mod download;
pub mod manifest;
pub mod model_store;
pub mod provider;
pub mod vector_index;
pub mod worker_proto;

pub use manifest::{
    embedded_manifest, model_spec, verify_package, verify_package_with_spec, Manifest,
    ManifestFile, ManifestModel, ManifestPackage, ManifestSource, ModelSpec, PackageVerification,
    MODEL_IDS, MODEL_RURI_V3_30M, MODEL_RURI_V3_70M,
};
pub use model_store::{
    delete_package, resolve_models_dir, resolve_models_dir_from_env, scan_models,
    verified_package_dir, ModelPackageState, ModelsDirOptions,
};
pub use provider::{
    is_known_model, mean_pool_l2, EmbeddingProvider, ModelInfo, ProviderError, RuriOnnxProvider,
};
pub use vector_index::{
    index_files_exist, remove_index_files, IndexIdentity, Neighbor, VectorIndex, VectorIndexError,
    DEFAULT_CONNECTIVITY, DEFAULT_EXPANSION_ADD, DEFAULT_EXPANSION_SEARCH,
};
pub use worker_proto::{
    read_frame, write_frame, ModelInfo as ProtoModelInfo, WorkerRequest, WorkerResponse,
    WorkerResult, WorkerStatus, MAX_FRAME_BYTES,
};
