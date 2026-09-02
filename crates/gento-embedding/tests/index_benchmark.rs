//! USearch索引の5万件パフォーマンス計測 (WP-12 acceptance 1)。
//!
//! - 合成50,000ベクトル (384次元 = ruri-v3-70mと同一。**クラスタ構造付き** —
//!   実テキスト埋め込みは多様体構造を持つため、無作為一様ベクトル
//!   (HNSWの最悪ケース) よりも実データを代表する) で構築・検索時間・
//!   メモリ・再現率を計測する。実測値は docs/dev/measurements/
//!   wp12-usearch-50k.md に記録 (一様乱数での最悪ケース計測も同文書)。
//! - release測定が正本 (`cargo test --release -p gento-embedding
//!   --test index_benchmark -- --nocapture`)。

use std::collections::HashSet;
use std::time::Instant;

use gento_embedding::vector_index::{IndexIdentity, VectorIndex};

/// 計測規模 (acceptance: 5万ベクトル)。
const TOTAL: usize = 50_000;
/// 次元 (ruri-v3-70mと同一)。
const DIMS: usize = 384;
/// 1クエリの取得件数 (ハイブリッド検索の意味側上位Kと同一)。
const TOP_K: usize = 200;
/// クラスタ数 (100クラスタ×500ベクトル)。
const CLUSTERS: usize = 100;

/// xorshift64* — 決定的な疑似乱数 (rand依存を避ける)。
struct Rng(u64);

impl Rng {
    fn next_f32(&mut self) -> f32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        ((self.0.wrapping_mul(0x2545F4914F6CDD1D) >> 40) as f32) / (1u64 << 24) as f32
    }

    fn unit_vector(&mut self, dims: usize) -> Vec<f32> {
        let mut vector: Vec<f32> = (0..dims).map(|_| self.next_f32() * 2.0 - 1.0).collect();
        let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
        for value in &mut vector {
            *value /= norm;
        }
        vector
    }

    /// クラスタ重心 + 小摂動 (摂動比率で広がりを制御)。
    fn near(&mut self, centroid: &[f32], spread: f32, dims: usize) -> Vec<f32> {
        let mut vector: Vec<f32> = (0..dims)
            .map(|i| centroid[i] + (self.next_f32() - 0.5) * spread)
            .collect();
        let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
        for value in &mut vector {
            *value /= norm;
        }
        vector
    }
}

#[test]
fn index_benchmark_50k_search_build_memory_recall() {
    let mut rng = Rng(0x5EED_1234_ABCD_0001);
    let centroids: Vec<Vec<f32>> = (0..CLUSTERS).map(|_| rng.unit_vector(DIMS)).collect();
    let vectors: Vec<Vec<f32>> = (0..TOTAL)
        .map(|i| rng.near(&centroids[i % CLUSTERS], 0.25, DIMS))
        .collect();
    // クエリは各クラスタ重心付近 (期待: 同クラスタが上位)
    let queries: Vec<(usize, Vec<f32>)> = (0..20)
        .map(|q| {
            let cluster = (q * 7) % CLUSTERS;
            (cluster, rng.near(&centroids[cluster], 0.2, DIMS))
        })
        .collect();

    // ===== 構築 =====
    let identity = IndexIdentity {
        model_name: "benchmark-synthetic".to_string(),
        model_revision: "bench".to_string(),
        dimensions: DIMS,
    };
    let index = VectorIndex::new(identity, TOTAL).expect("index");
    let started = Instant::now();
    for (asset_id, vector) in vectors.iter().enumerate() {
        index.add(asset_id as i64, vector).expect("add");
    }
    let build_secs = started.elapsed().as_secs_f64();
    assert_eq!(index.len(), TOTAL);
    let memory = index.memory_usage();
    println!(
        "[wp12-bench] build: {TOTAL} vectors x {DIMS} dims in {build_secs:.2}s ({:.0} adds/s)",
        TOTAL as f64 / build_secs
    );
    println!(
        "[wp12-bench] memory_usage: {memory} bytes ({:.1} MiB, {:.1} bytes/vector)",
        memory as f64 / 1024.0 / 1024.0,
        memory as f64 / TOTAL as f64
    );
    // 純ベクトル分は 50000*384*4 = 73.4MiB。グラフ込みで10倍を超えたら
    // 設定ミス (容量見積りの健全性上限)。
    assert!(memory < TOTAL * DIMS * 4 * 10);
    // 構築時間の健全性上限 (release実測値はmeasurementsに記録)
    if cfg!(debug_assertions) {
        assert!(build_secs < 240.0, "debug構築が遅すぎ: {build_secs}s");
    } else {
        assert!(build_secs < 60.0, "release構築が遅すぎ: {build_secs}s");
    }

    // ===== 検索 (acceptance: 5万ベクトルで検索<0.5秒) =====
    let mut timings: Vec<f64> = Vec::new();
    for (qi, (_, query)) in queries.iter().enumerate() {
        let started = Instant::now();
        let hits = index.search(query, TOP_K).expect("search");
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        timings.push(elapsed_ms);
        assert_eq!(hits.len(), TOP_K, "クエリ{qi}: 上位Kが返る");
        if qi == 0 {
            println!("[wp12-bench] first query: {elapsed_ms:.2}ms");
        }
    }
    timings.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p50 = timings[timings.len() / 2];
    let p95 = timings[(timings.len() as f64 * 0.95) as usize];
    let max = timings[timings.len() - 1];
    println!(
        "[wp12-bench] search (n={} k={TOP_K}): p50={p50:.2}ms p95={p95:.2}ms max={max:.2}ms",
        timings.len()
    );
    // acceptance: 5万ベクトルで1クエリ0.5秒未満 (release正本。debugは
    // 最適化なしでも10倍猶予の健全性上限)。
    if cfg!(debug_assertions) {
        assert!(max < 5_000.0, "debug測定でも5秒は超えない想定: {max:.2}ms");
    } else {
        assert!(max < 500.0, "acceptance違反: 1クエリが0.5秒超: {max:.2}ms");
    }

    // ===== 再現率 (近似 vs 全件照合。クラスタ構造データ) =====
    let mut recalls: Vec<f64> = Vec::new();
    let mut cluster_purities: Vec<f64> = Vec::new();
    for (_, query) in &queries {
        let approx: Vec<i64> = index
            .search(query, 10)
            .unwrap()
            .iter()
            .map(|n| n.key)
            .collect();
        let mut scored: Vec<(i64, f32)> = vectors
            .iter()
            .enumerate()
            .map(|(asset_id, vector)| {
                let similarity = query.iter().zip(vector).map(|(a, b)| a * b).sum::<f32>();
                (asset_id as i64, similarity)
            })
            .collect();
        // 同点はid昇順で決定的に
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });
        let exact_top: HashSet<i64> = scored.iter().take(10).map(|(id, _)| *id).collect();
        let overlap = approx.iter().filter(|id| exact_top.contains(*id)).count();
        recalls.push(overlap as f64 / 10.0);
        // 上位10件が同クラスタに属する割合 (検索品質の実用指標)
        let same_cluster = approx
            .iter()
            .filter(|id| (**id as usize) % CLUSTERS == scored[0].0 as usize % CLUSTERS)
            .count();
        cluster_purities.push(same_cluster as f64 / 10.0);
    }
    let mut sorted_recalls = recalls.clone();
    sorted_recalls.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let avg_recall: f64 = recalls.iter().sum::<f64>() / recalls.len() as f64;
    let avg_purity: f64 = cluster_purities.iter().sum::<f64>() / cluster_purities.len() as f64;
    println!(
        "[wp12-bench] recall@10 vs exact: avg={avg_recall:.3} min={:.3} / cluster purity={avg_purity:.3}",
        sorted_recalls[0]
    );
    assert!(
        avg_recall >= 0.85,
        "クラスタ構造データでrecall@10が低すぎ: {avg_recall:.3}"
    );
    assert!(
        avg_purity >= 0.95,
        "上位10件のクラスタ純度が低い: {avg_purity:.3}"
    );

    // ===== 構造フィルタ付き検索 (ハイブリッドの意味側) =====
    let allowed: HashSet<i64> = (0..TOTAL).step_by(2).map(|id| id as i64).collect();
    let started = Instant::now();
    let filtered = index
        .filtered_search(&queries[0].1, TOP_K, &allowed)
        .expect("filtered");
    let filtered_ms = started.elapsed().as_secs_f64() * 1000.0;
    println!("[wp12-bench] filtered search (half of 50k allowed): {filtered_ms:.2}ms");
    assert!(filtered.iter().all(|n| allowed.contains(&n.key)));
    assert!(!filtered.is_empty());
    if !cfg!(debug_assertions) {
        assert!(
            filtered_ms < 500.0,
            "フィルタ付きもacceptance内: {filtered_ms:.2}ms"
        );
    }

    // ===== roundtrip保存 (永続化のオーバヘッド計測) =====
    let dir = std::env::temp_dir().join(format!(
        "gento-bench-vidx-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let started = Instant::now();
    index.save(&dir).expect("save");
    let save_secs = started.elapsed().as_secs_f64();
    let file_size = std::fs::metadata(dir.join("index.usearch"))
        .map(|m| m.len())
        .unwrap_or(0);
    println!(
        "[wp12-bench] save: {save_secs:.2}s, file={file_size} bytes ({:.1} MiB)",
        file_size as f64 / 1024.0 / 1024.0
    );
    let started = Instant::now();
    let reopened = VectorIndex::open(
        &dir,
        &IndexIdentity {
            model_name: "benchmark-synthetic".to_string(),
            model_revision: "bench".to_string(),
            dimensions: DIMS,
        },
    )
    .expect("reopen");
    let load_secs = started.elapsed().as_secs_f64();
    println!(
        "[wp12-bench] load: {load_secs:.2}s ({} vectors)",
        reopened.len()
    );
    assert_eq!(reopened.len(), TOTAL);
    // 再読込後も検索可能 (起動時再読込acceptance)
    let hits = reopened.search(&queries[0].1, TOP_K).unwrap();
    assert_eq!(hits.len(), TOP_K);
    let _ = std::fs::remove_dir_all(&dir);
}
