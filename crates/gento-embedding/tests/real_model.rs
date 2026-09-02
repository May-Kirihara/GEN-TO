//! 実モデル (.local-models) を使ったprovider検証 (WP-11 acceptance)。
//!
//! `GENTO_EMBEDDING_MODELS_DIR` 環境変数でmodels_dirを指定した場合のみ
//! 実行 (通常のcargo testではスキップ — モデルは274MB/147MBでgit管理外)。
//!
//! ```sh
//! GENTO_EMBEDDING_MODELS_DIR=.local-models cargo test -p gento-embedding --test real_model
//! ```

use gento_embedding::manifest::MODEL_RURI_V3_70M;
use gento_embedding::provider::{EmbeddingProvider, RuriOnnxProvider};

/// models_dir解決: 相対パスはworkspaceルート (CARGO_MANIFEST_DIR/..) 基準。
/// cargo testのCWDはcrateルートのため、`.local-models` 等の指定に対応する。
fn resolve_models_dir() -> std::path::PathBuf {
    let dir =
        std::env::var("GENTO_EMBEDDING_MODELS_DIR").expect("GENTO_EMBEDDING_MODELS_DIR 未設定");
    let path = std::path::PathBuf::from(&dir);
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

#[test]
fn real_model_embedding_end_to_end() {
    let Ok(_env_dir) = std::env::var("GENTO_EMBEDDING_MODELS_DIR") else {
        eprintln!("GENTO_EMBEDDING_MODELS_DIR 未設定 — スキップ");
        return;
    };
    let dir = resolve_models_dir();
    let verification = gento_embedding::manifest::verify_package(&dir, MODEL_RURI_V3_70M);
    assert!(
        verification.is_verified(),
        "パッケージ検証: {}",
        verification.describe()
    );
    let mut provider =
        RuriOnnxProvider::load(MODEL_RURI_V3_70M, &dir.join(MODEL_RURI_V3_70M), Some(2))
            .expect("モデルロード");

    let info = provider.model_info();
    assert_eq!(info.embedding_dim, 384);
    assert_eq!(info.max_input_length, 8192);

    // 意味近傍の検証 (acceptance 5-3の土台): 近い文書対は遠い対より高いcosine
    let query = "猫が窓辺で日向ぼっこをしている写真";
    let near_doc = "窓際で猫が日光浴している。暖かい日に窓辺で丸まっている猫の画像。";
    let far_doc = "宇宙船が火星の表面に着陸するSFイラスト。エイリアンの宇宙基地。";

    let start = std::time::Instant::now();
    let q = provider.embed_query(query).expect("query");
    let query_latency = start.elapsed();
    let docs = provider
        .embed_documents(&[near_doc.to_string(), far_doc.to_string()])
        .expect("docs");

    // L2正規化の検証
    for (label, v) in [("query", &q), ("near", &docs[0]), ("far", &docs[1])] {
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 1e-3,
            "{label} のL2ノルムが1でない: {norm}"
        );
        assert_eq!(v.len(), 384, "{label} の次元");
    }
    let cosine = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
    let near_score = cosine(&q, &docs[0]);
    let far_score = cosine(&q, &docs[1]);
    println!("query latency: {query_latency:?}");
    println!("cos(query, near) = {near_score:.4}");
    println!("cos(query, far)  = {far_score:.4}");
    assert!(
        near_score > far_score + 0.1,
        "近い文書対の方が高い類似度であるべき: near={near_score:.4} far={far_score:.4}"
    );
}

#[test]
fn real_model_document_prefix_affects_embedding() {
    let Ok(_env_dir) = std::env::var("GENTO_EMBEDDING_MODELS_DIR") else {
        eprintln!("GENTO_EMBEDDING_MODELS_DIR 未設定 — スキップ");
        return;
    };
    let dir = resolve_models_dir();
    let mut provider =
        RuriOnnxProvider::load(MODEL_RURI_V3_70M, &dir.join(MODEL_RURI_V3_70M), Some(2))
            .expect("モデルロード");
    let text = "同一文書";
    let as_query = provider.embed_query(text).expect("query");
    let as_doc = provider
        .embed_documents(&[text.to_string()])
        .expect("doc")
        .into_iter()
        .next()
        .unwrap();
    let dot: f32 = as_query.iter().zip(&as_doc).map(|(a, b)| a * b).sum();
    println!("cos(query-prefix, doc-prefix) = {dot:.4}");
    // 接頭辞が異なれば埋め込みも(一般に)変化する — 完全一致は逆に怪しい
    assert!(dot < 0.999999, "query/doc接頭辞で埋め込みが変わる: {dot}");
}
