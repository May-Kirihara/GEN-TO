//! WP-12ハイブリッド検索の実モデル評価 (acceptance 3の土台)。
//!
//! `GENTO_EMBEDDING_MODELS_DIR` 設定時のみ実行。WP-11評価セット (30文書+
//! 10クエリ) を実ruri-v3ベクトルへ埋め込み、以下を検証する:
//!
//! 1. **USearch索引の再現率**: 索引top-3が総当たりcosineのtop-3と一致
//!    (30件規模ではHNSWは実質正確 — 索引化で品質を落とさないことの保証)
//! 2. **総当たりRecall@3 ≥ 0.80** (WP-11評価との整合)
//! 3. **RRF融合の重み挙動** (実ベクトルの順位リストで):
//!    w=1 → 意味順、w=0 → キーワード順、中間 → 両側一致が最上位
//! 4. **類似画像検索**: docベクトルをクエリにした近傍が同トピック
//!
//! ```sh
//! GENTO_EMBEDDING_MODELS_DIR=.local-models \
//!   cargo test --release -p gento-embedding --test hybrid_eval -- --nocapture
//! ```

mod eval_corpus;

use std::collections::HashSet;

use eval_corpus::{corpus, queries};
use gento_embedding::manifest::{model_spec, MODEL_RURI_V3_70M};
use gento_embedding::provider::{EmbeddingProvider, RuriOnnxProvider};
use gento_embedding::vector_index::{IndexIdentity, VectorIndex};
use gento_search::{weighted_rrf, FusedRank};

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

/// 総当たりcosineの上位k件 (id, 類似度)。
fn brute_top_k(query: &[f32], docs: &[(i64, Vec<f32>)], k: usize) -> Vec<(i64, f32)> {
    let mut scored: Vec<(i64, f32)> = docs
        .iter()
        .map(|(id, vector)| (*id, cosine(query, vector)))
        .collect();
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    scored.truncate(k);
    scored
}

#[test]
fn hybrid_eval_real_model_index_recall_and_rrf() {
    let Ok(_env) = std::env::var("GENTO_EMBEDDING_MODELS_DIR") else {
        eprintln!("GENTO_EMBEDDING_MODELS_DIR 未設定 — スキップ");
        return;
    };
    let models_dir = resolve_models_dir();
    let corpus = corpus();
    let queries = queries();
    let spec = model_spec(MODEL_RURI_V3_70M).expect("spec");

    let mut provider = RuriOnnxProvider::load(
        MODEL_RURI_V3_70M,
        &models_dir.join(MODEL_RURI_V3_70M),
        Some(2),
    )
    .expect("モデルロード");

    let texts: Vec<String> = corpus.iter().map(|(doc, _)| doc.to_string()).collect();
    let doc_vectors = provider.embed_documents(&texts).expect("doc embed");
    let query_vectors: Vec<Vec<f32>> = queries
        .iter()
        .map(|query| provider.embed_query(query).expect("query embed"))
        .collect();
    // 索引対象はasset id相当 (1..=30 — 0はクエリ側と区別しない意味での慣例)
    let docs: Vec<(i64, Vec<f32>)> = doc_vectors
        .iter()
        .enumerate()
        .map(|(index, vector)| ((index + 1) as i64, vector.clone()))
        .collect();

    // ===== USearch索引 (WP-12) =====
    let identity = IndexIdentity {
        model_name: MODEL_RURI_V3_70M.to_string(),
        model_revision: spec.revision,
        dimensions: spec.embedding_dim,
    };
    let index = VectorIndex::new(identity.clone(), docs.len()).expect("index");
    for (id, vector) in &docs {
        index.add(*id, vector).expect("add");
    }
    assert_eq!(index.len(), docs.len());
    // identity違い (30m = 256次元) では開けないことを焦慮しない —
    // 混在拒否はvector_index unitテストの担当。ここでは実モデル寸法で
    // 索引化品質を測る。

    // ===== 1/2: 索引再現率 + 総当たりRecall@3 =====
    let mut index_top3_matches_brute = 0usize;
    let mut comparisons = 0usize;
    let mut total_relevant = 0usize;
    let mut total_hit = 0usize;
    for (query_index, query_vector) in query_vectors.iter().enumerate() {
        let brute = brute_top_k(query_vector, &docs, 3);
        let via_index: Vec<(i64, f32)> = index
            .search(query_vector, 3)
            .expect("search")
            .into_iter()
            .map(|neighbor| (neighbor.key, neighbor.similarity))
            .collect();
        // 1) 索引top-3 == 総当たりtop-3 (順序含め。30件では完全一致が期待)
        let brute_ids: Vec<i64> = brute.iter().map(|(id, _)| *id).collect();
        let index_ids: Vec<i64> = via_index.iter().map(|(id, _)| *id).collect();
        if brute_ids == index_ids {
            index_top3_matches_brute += 1;
        }
        comparisons += 1;
        // 類似度の数値も総当たりcosineと一致する (cos metric = 1-cos)
        for ((_, brute_sim), (_, index_sim)) in brute.iter().zip(via_index.iter()) {
            assert!(
                (brute_sim - index_sim).abs() < 1e-4,
                "索引類似度がcosineと乖離: {brute_sim} vs {index_sim}"
            );
        }
        // 2) Recall@3 (総当たり = 正解の定義側)
        let relevant: Vec<i64> = corpus
            .iter()
            .enumerate()
            .filter(|(_, (_, qs))| qs.contains(&query_index))
            .map(|(i, _)| (i + 1) as i64)
            .collect();
        let hits = relevant.iter().filter(|r| index_ids.contains(r)).count();
        total_relevant += relevant.len();
        total_hit += hits;
    }
    let index_agreement = index_top3_matches_brute as f64 / comparisons as f64;
    let recall3 = total_hit as f64 / total_relevant as f64;
    println!("[wp12-eval] index top-3 == brute top-3: {index_agreement:.2}");
    println!("[wp12-eval] Recall@3 via index: {recall3:.4}");
    assert!(
        index_agreement >= 0.95,
        "索引化で順位品質が落ちている: {index_agreement:.2}"
    );
    assert!(recall3 >= 0.80, "Recall@3が閾値割れ: {recall3:.4}");

    // ===== 3: RRF重み挙動 (実ランキングリスト) =====
    let query_vector = &query_vectors[0]; // 「猫がくつろいでいる画像」
    let semantic: Vec<(i64, f32)> = index
        .search(query_vector, 5)
        .unwrap()
        .into_iter()
        .map(|n| (n.key, n.similarity))
        .collect();
    // キーワード側 (FTS相当) の代理: 意味順とは無関係な決定的順序
    // (id降順の上位5 — 意味側と意図的に重ならない対象を選ぶ)
    let keyword: Vec<i64> = (1..=30i64).rev().take(5).collect();
    // w=1: 意味順そのもの
    let fused_w1 = weighted_rrf(&keyword, &semantic, 1.0);
    let semantic_order: Vec<i64> = semantic.iter().map(|(id, _)| *id).collect();
    let top_semantic: Vec<i64> = fused_w1
        .iter()
        .filter(|f| f.semantic_rank.is_some())
        .take(5)
        .map(|f| f.asset_id)
        .collect();
    assert_eq!(top_semantic, semantic_order, "w=1は意味順");
    // w=0: キーワード順が先頭
    let fused_w0 = weighted_rrf(&keyword, &semantic, 0.0);
    let top_keyword: Vec<i64> = fused_w0.iter().take(5).map(|f| f.asset_id).collect();
    assert_eq!(top_keyword, keyword, "w=0はキーワード順");
    // 中間 (w=0.5): 両リストに存在するasset (あれば) が単独側より上位
    let overlap: Vec<i64> = keyword
        .iter()
        .filter(|id| semantic.iter().any(|(sid, _)| sid == *id))
        .copied()
        .collect();
    let fused_mid = weighted_rrf(&keyword, &semantic, 0.5);
    if let Some(&both) = overlap.first() {
        let rank_of =
            |fused: &Vec<FusedRank>, id: i64| fused.iter().position(|f| f.asset_id == id).unwrap();
        let only_keyword: Vec<i64> = keyword
            .iter()
            .filter(|id| !semantic.iter().any(|(sid, _)| sid == *id))
            .copied()
            .collect();
        if let Some(&only) = only_keyword.first() {
            assert!(
                rank_of(&fused_mid, both) < rank_of(&fused_mid, only),
                "両側一致 {both} はキーワードのみの {only} より上位: {:?}",
                fused_mid
            );
        }
    }
    println!("[wp12-eval] rrf weight behavior: w1=semantic order, w0=keyword order OK");

    // ===== 4: 類似画像検索 (docベクトルをクエリにした近傍) =====
    // 「同トピック」ラベルは完全な正解ではない (例: 「シルエット写真」は
    // 夜景トピックと意味近傍になり得る — それは正しい挙動)。 therefore
    // top-1一致率は緩めに設定し、top-3内に同トピックを含む率で補強する。
    // 無作為なら top-1 ≈ 2/29 ≈ 7%。
    let mut similar_same_topic = 0usize;
    let mut similar_top3_contains = 0usize;
    for (doc_index, (_, relevant_queries)) in corpus.iter().enumerate() {
        let query_vector = &doc_vectors[doc_index];
        let self_id = (doc_index + 1) as i64;
        let neighbors = index.search(query_vector, 4).unwrap();
        let topic_ids: HashSet<i64> = corpus
            .iter()
            .enumerate()
            .filter(|(_, (_, rq))| {
                !rq.is_empty() && relevant_queries.iter().any(|q| rq.contains(q))
            })
            .map(|(i, _)| (i + 1) as i64)
            .collect::<HashSet<_>>();
        let others: Vec<i64> = neighbors
            .iter()
            .filter(|n| n.key != self_id)
            .take(3)
            .map(|n| n.key)
            .collect();
        if topic_ids.contains(&others[0]) {
            similar_same_topic += 1;
        }
        if others.iter().any(|id| topic_ids.contains(id)) {
            similar_top3_contains += 1;
        }
    }
    let similar_ratio = similar_same_topic as f64 / corpus.len() as f64;
    let top3_ratio = similar_top3_contains as f64 / corpus.len() as f64;
    println!("[wp12-eval] similar-image top-1 same-topic: {similar_ratio:.2} (top-3 contains: {top3_ratio:.2})");
    assert!(
        similar_ratio >= 0.60,
        "類似画像検索のtop-1同トピック率が低い: {similar_ratio:.2}"
    );
    assert!(
        top3_ratio >= 0.85,
        "類似画像検索のtop-3に同トピックが無い率が高い: {top3_ratio:.2}"
    );
}
