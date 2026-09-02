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

use gento_embedding::manifest::MODEL_RURI_V3_70M;
use gento_embedding::provider::{EmbeddingProvider, RuriOnnxProvider};

/// Recall@3の合格閾値 (実測に基づく設定。docs/dev/embedding-eval.md参照)。
const RECALL_THRESHOLD: f64 = 0.80;

/// 合成検索文書30件 (画像メタデータ由来を模した日本語文書)。
/// (id, 文書, 正解クエリ番号のリスト)
fn corpus() -> Vec<(&'static str, Vec<usize>)> {
    vec![
        // 0: 猫 (query 0)
        (
            "窓辺で日向ぼっこをしている猫の写真。暖かい日光の中で丸まっている。",
            vec![0],
        ),
        (
            "子猫が毛布の上で眠っているイラスト。かわいい寝顔。",
            vec![0],
        ),
        ("茶トラの猫がキャットタワーで遊んでいる。", vec![0]),
        // 3: 犬 (query 1)
        (
            "公園でフリスビーを追いかける犬。元気に走るゴールデンレトリバー。",
            vec![1],
        ),
        ("柴犬が散歩中に座り込んでいる写真。首輪とリード。", vec![1]),
        ("庭でボール遊びをする子犬のイラスト。", vec![1]),
        // 6: 夜景 (query 2)
        (
            "夜の東京タワーと街の灯り。展望台から見た夜景写真。",
            vec![2],
        ),
        (
            "ネオンサインが光る繁華街の夜。雨に濡れた路面の反射。",
            vec![2],
        ),
        (
            "夜景を背景にした都市のシルエットイラスト。ビル群の窓の光。",
            vec![2],
        ),
        // 9: 海/ビーチ (query 3)
        (
            "夏のビーチで波打ち際を走る人々。青い空と白い砂浜。",
            vec![3],
        ),
        (
            "夕暮れの海に沈む夕日。サーファーが波に乗っている。",
            vec![3],
        ),
        ("南の島の海岸線。透き通るエメラルドグリーンの海。", vec![3]),
        // 12: 食べ物 (query 4)
        (
            "テーブルに並ぶラーメンのボウル。チャーシューと半熟卵のトッピング。",
            vec![4],
        ),
        (
            "手作りのパンケーキにメイプルシロップをかける。朝食の写真。",
            vec![4],
        ),
        ("カフェで飲むカフェラテ。ラテアートのハート模様。", vec![4]),
        // 15: 電車/乗り物 (query 5)
        (
            "夕暮れの鉄道駅に停車する列車。ホームと架線のシルエット。",
            vec![5],
        ),
        ("山間を走るローカル線の電車。車窓から見える紅葉。", vec![5]),
        ("新幹線が線路を高速で走行する写真。", vec![5]),
        // 18: 花 (query 6)
        ("春の桜並木。満開のピンク色の花びらが咲く。", vec![6]),
        ("ひまわり畑の写真。夏の青空に黄色い花が並ぶ。", vec![6]),
        ("バラの花の接写。赤い花びらと露の水滴。", vec![6]),
        // 21: 山/自然 (query 7)
        ("冬の山々に雪が積もった風景。アルプスの山岳写真。", vec![7]),
        ("朝焼けの山頂から雲海を見下ろす。登山の風景。", vec![7]),
        ("紅葉した渓谷と清流。秋の日本の山の風景。", vec![7]),
        // 24: SF/ファンタジー (query 8)
        (
            "宇宙船が火星の表面に着陸するSFイラスト。エイリアンの基地。",
            vec![8],
        ),
        (
            "魔法使いが杖を振り、光の魔法を放つファンタジーイラスト。",
            vec![8],
        ),
        (
            "未来的なサイバーパンク都市。ホログラムの看板と雨。",
            vec![8],
        ),
        // 27: 人物ポートレート (query 9)
        (
            "スタジオで撮影された女性のポートレート。柔らかい照明。",
            vec![9],
        ),
        ("街角で笑顔の男性をスナップ撮影。自然な表情。", vec![9]),
        ("逆光の中に立つ人物のシルエット写真。", vec![9]),
    ]
}

/// 評価クエリ10件 (id, クエリテキスト)。
fn queries() -> Vec<&'static str> {
    vec![
        "猫がくつろいでいる画像",
        "犬が外で遊んでいる写真",
        "きれいな夜景を見たい",
        "海やビーチの風景",
        "おいしそうな食べ物の写真",
        "電車や鉄道の写真",
        "花がきれいに咲いている画像",
        "山の壮大な自然の風景",
        "SFやファンタジーの世界観のイラスト",
        "人物のポートレート写真",
    ]
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
