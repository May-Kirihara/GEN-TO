//! Rust実装と公式実装 (SentenceTransformers) のベクトル一致検証 (WP-11)。
//!
//! `tests/fixtures/ruri70m_reference.json` は CONV-1のvenv
//! (sentence-transformers 6.0.1 / 元モデルrevision固定) が生成した参照
//! ベクトル。Rust実装 (tokenizers→ort→mean pooling→L2) がこれと高い
//! cosine一致を保つことで、実装ドリフト (特殊トークン欠落・pooling誤り等)
//! を回帰検出する。
//!
//! `GENTO_EMBEDDING_MODELS_DIR` 設定時のみ実行。

use gento_embedding::manifest::MODEL_RURI_V3_70M;
use gento_embedding::provider::{EmbeddingProvider, RuriOnnxProvider};
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    query_text: String,
    query_vector: Vec<f32>,
    documents: Vec<String>,
    document_vectors: Vec<Vec<f32>>,
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>()
}

#[test]
fn rust_embeddings_agree_with_reference() {
    let Ok(_env) = std::env::var("GENTO_EMBEDDING_MODELS_DIR") else {
        eprintln!("GENTO_EMBEDDING_MODELS_DIR 未設定 — スキップ");
        return;
    };
    let fixture: Fixture =
        serde_json::from_str(include_str!("fixtures/ruri70m_reference.json")).expect("fixture解析");
    let models_dir = resolve_models_dir();
    let mut provider = RuriOnnxProvider::load(
        MODEL_RURI_V3_70M,
        &models_dir.join(MODEL_RURI_V3_70M),
        Some(2),
    )
    .expect("モデルロード");

    let query = provider.embed_query(&fixture.query_text).expect("query");
    let docs = provider.embed_documents(&fixture.documents).expect("docs");

    // 許容誤差: 実測min 0.9999993 (ort 1.28静的リンクとtorch 2.14 CPUの
    // カーネル差 ~1e-6)。実装バグは大きく劣化するため確実に検出できる水準
    // (特殊トークン欠落時の実測: 0.966。プレフィックス欠落時: ~0.96)。
    let min_cos = 0.999_999f32;
    let query_cos = cosine(&query, &fixture.query_vector);
    println!("cos(query, reference) = {query_cos:.7}");
    assert!(
        query_cos >= min_cos,
        "クエリベクトルが参照と不一致: {query_cos:.7} < {min_cos}"
    );
    for (index, (vector, reference)) in docs.iter().zip(&fixture.document_vectors).enumerate() {
        let cos = cosine(vector, reference);
        println!("cos(doc[{index}], reference) = {cos:.7}");
        assert!(
            cos >= min_cos,
            "文書{index}のベクトルが参照と不一致: {cos:.7} < {min_cos}"
        );
    }
}

fn resolve_models_dir() -> std::path::PathBuf {
    let path = std::path::PathBuf::from(
        std::env::var("GENTO_EMBEDDING_MODELS_DIR").expect("GENTO_EMBEDDING_MODELS_DIR"),
    );
    if path.is_absolute() {
        path
    } else {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root")
            .join(path)
    }
}
