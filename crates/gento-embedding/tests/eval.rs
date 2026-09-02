//! 既定モデルの類似妥当性評価 (WP-11 acceptance 5-3)。
//!
//! `GENTO_EMBEDDING_MODELS_DIR` 設定時のみ実行。日本語の近い/遠い文書
//! ペアを含む合成検索文書30件 + 10クエリで Recall@3 を測定し、
//! 閾値 (RECALL_THRESHOLD) を下回れば失敗する。実測値・閾値は
//! docs/dev/embedding-eval.md に記録する。
//!
//! ```sh
//! GENTO_EMBEDDING_MODELS_DIR=.local-models \
//!   cargo test --release -p gento-embedding --test eval -- --nocapture
//! ```

mod eval_corpus;

use eval_corpus::{corpus, queries};
use gento_embedding::manifest::MODEL_RURI_V3_70M;
use gento_embedding::provider::{EmbeddingProvider, RuriOnnxProvider};

/// Recall@3の合格閾値 (実測に基づく設定。docs/dev/embedding-eval.md参照)。
const RECALL_THRESHOLD: f64 = 0.80;

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

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>()
}

#[test]
fn recall_at_3_for_default_model() {
    let Ok(_env) = std::env::var("GENTO_EMBEDDING_MODELS_DIR") else {
        eprintln!("GENTO_EMBEDDING_MODELS_DIR 未設定 — スキップ");
        return;
    };
    let models_dir = resolve_models_dir();
    let corpus = corpus();
    let queries = queries();
    assert_eq!(corpus.len(), 30, "評価セットは30文書");
    assert_eq!(queries.len(), 10);

    let mut provider = RuriOnnxProvider::load(
        MODEL_RURI_V3_70M,
        &models_dir.join(MODEL_RURI_V3_70M),
        Some(2),
    )
    .expect("モデルロード");

    // クエリ埋め込み (1件ずつ = embed_queryのレイテンシ実測も兼ねる)
    let mut query_vectors = Vec::new();
    let mut query_latencies = Vec::new();
    for query in &queries {
        let start = std::time::Instant::now();
        let vector = provider.embed_query(query).expect("query embed");
        query_latencies.push(start.elapsed());
        query_vectors.push(vector);
    }

    // 文書埋め込み (バッチ — スループット実測)
    let texts: Vec<String> = corpus.iter().map(|(doc, _)| doc.to_string()).collect();
    let doc_start = std::time::Instant::now();
    let doc_vectors = provider.embed_documents(&texts).expect("doc embed");
    let doc_elapsed = doc_start.elapsed();

    // Recall@3測定
    let mut total_relevant = 0usize;
    let mut total_hit = 0usize;
    let mut per_query = Vec::new();
    for (query_index, query_vector) in query_vectors.iter().enumerate() {
        let mut scored: Vec<(usize, f32)> = doc_vectors
            .iter()
            .enumerate()
            .map(|(doc_index, vector)| (doc_index, cosine(query_vector, vector)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let top3: Vec<usize> = scored.iter().take(3).map(|(i, _)| *i).collect();
        let relevant: Vec<usize> = corpus
            .iter()
            .enumerate()
            .filter(|(_, (_, qs))| qs.contains(&query_index))
            .map(|(i, _)| i)
            .collect();
        let hits = relevant.iter().filter(|r| top3.contains(r)).count();
        total_relevant += relevant.len();
        total_hit += hits;
        per_query.push((query_index, hits, relevant.len()));
        println!(
            "query {:2} {:<24} top3={:?} relevant={:?} hits={}/{}",
            query_index,
            queries[query_index],
            top3,
            relevant,
            hits,
            relevant.len()
        );
    }
    let recall = total_hit as f64 / total_relevant as f64;
    let avg_query_ms = query_latencies
        .iter()
        .map(|d| d.as_secs_f64() * 1000.0)
        .sum::<f64>()
        / queries.len() as f64;
    let max_query_ms = query_latencies
        .iter()
        .map(|d| d.as_secs_f64() * 1000.0)
        .fold(f64::MIN, f64::max);
    println!("Recall@3 = {recall:.4} ({total_hit}/{total_relevant}) — 閾値 {RECALL_THRESHOLD}");
    println!("クエリレイテンシ: avg {avg_query_ms:.1}ms / max {max_query_ms:.1}ms");
    println!(
        "文書バッチ ({}件): {:?} ({:.1} ms/件)",
        texts.len(),
        doc_elapsed,
        doc_elapsed.as_secs_f64() * 1000.0 / texts.len() as f64
    );
    assert!(
        recall >= RECALL_THRESHOLD,
        "Recall@3 {recall:.4} が閾値 {RECALL_THRESHOLD} を下回りました: {per_query:?}"
    );
}
