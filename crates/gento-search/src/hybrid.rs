//! 重み付きRRF (Reciprocal Rank Fusion) ハイブリッド検索融合 (MVP WP-12)。
//!
//! # 設計 (api-contract §46固定)
//!
//! キーワード順位リスト (FTS bm25順) と意味順位リスト (USearch類似度順) を
//! 重み付きRRFで融合する:
//!
//! ```text
//! score(d) = w · 1/(k + rank_sem(d)) + (1-w) · 1/(k + rank_kw(d))
//!   k = 60 (RRF標準値・固定)
//!   w ∈ [0,1] (意味重み。w=0 キーワードのみ / w=1 意味のみ)
//!   rank = 各リスト内の1開始順位。リストに不在 = 項0
//! ```
//!
//! - 並びは score 降順、同点は asset id 昇順 (決定性保証)。
//! - 候補集合は呼び出し側が決める (構造フィルタ適用済みの集合)。
//!   `w > 0` では意味側のみヒットしたassetも候補に含む設計
//!   (自然言語クエリはFTS一致ゼロでも意味側で発見できる必要がある)。
//! - `w == 0` のハイブリッドは呼び出し側が旧キーワード経路へフォール
//!   バックする (本moduleは呼ばれない — 旧順序との完全互換のため)。
//!
//! このmoduleはusearchにもDBにも依存しない (純関数 — 決定性テスト容易)。

/// RRFの平滑化定数 (標準値60で固定。api-contract §46)。
pub const RRF_K: f32 = 60.0;

/// ハイブリッド検索の融合済み結果1件。
#[derive(Debug, Clone, PartialEq)]
pub struct FusedRank {
    pub asset_id: i64,
    /// 融合スコア (w・(1-w)加重RRF。大きいほど上位)。
    pub score: f32,
    /// キーワード側の順位 (1開始。不在 = None)。
    pub keyword_rank: Option<u32>,
    /// 意味側の順位 (1開始。不在 = None)。
    pub semantic_rank: Option<u32>,
    /// 意味側のcosine類似度 (不在 = None。表示用)。
    pub semantic_similarity: Option<f32>,
}

/// 重みを[0,1]へ拘束する (UI入力の不正値防御)。
pub fn clamp_weight(weight: f32) -> f32 {
    weight.clamp(0.0, 1.0)
}

/// 重み付きRRF融合。
///
/// - `keyword`: キーワード順位リスト (上位から。asset id列)
/// - `semantic`: 意味順位リスト (上位から。asset id + cosine類似度)
/// - `weight`: 意味重み w ∈ [0,1] (範囲外はclamp)
///
/// 戻り値は score 降順・同点 asset id 昇順。両リストの和集合すべてを返す
/// (候補の間引きは呼び出し側)。
pub fn weighted_rrf(keyword: &[i64], semantic: &[(i64, f32)], weight: f32) -> Vec<FusedRank> {
    let weight = clamp_weight(weight);
    let mut fused: Vec<FusedRank> = Vec::with_capacity(keyword.len() + semantic.len());
    let kw_scale = 1.0 - weight;
    let sem_scale = weight;
    for (index, asset_id) in keyword.iter().enumerate() {
        let rank = (index + 1) as u32;
        // 重複id混入は最初の出現のみ採用 (呼び出し側は重複除去前提だが、
        // 混入時に二重計上して結果が爆発しないようここでも防御する)
        if fused.iter().any(|item| item.asset_id == *asset_id) {
            continue;
        }
        fused.push(FusedRank {
            asset_id: *asset_id,
            score: kw_scale / (RRF_K + rank as f32),
            keyword_rank: Some(rank),
            semantic_rank: None,
            semantic_similarity: None,
        });
    }
    for (index, (asset_id, similarity)) in semantic.iter().enumerate() {
        let rank = (index + 1) as u32;
        let contribution = sem_scale / (RRF_K + rank as f32);
        if let Some(existing) = fused.iter_mut().find(|item| item.asset_id == *asset_id) {
            existing.score += contribution;
            existing.semantic_rank = Some(rank);
            existing.semantic_similarity = Some(*similarity);
        } else {
            fused.push(FusedRank {
                asset_id: *asset_id,
                score: contribution,
                keyword_rank: None,
                semantic_rank: Some(rank),
                semantic_similarity: Some(*similarity),
            });
        }
    }
    fused.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.asset_id.cmp(&b.asset_id))
    });
    fused
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1/(60+rank) — テスト内の期待値計算用。
    fn rrf(rank: u32) -> f32 {
        1.0 / (RRF_K + rank as f32)
    }

    #[test]
    fn rrf_k_is_standard_60() {
        assert_eq!(RRF_K, 60.0);
    }

    #[test]
    fn math_exact_for_both_sides() {
        // kw: [10, 20, 30] / sem: [20 (0.9), 40 (0.8)] / w=0.5
        let fused = weighted_rrf(&[10, 20, 30], &[(20, 0.9), (40, 0.8)], 0.5);
        let by_id = |id: i64| fused.iter().find(|f| f.asset_id == id).unwrap();
        let a20 = by_id(20);
        assert!(
            (a20.score - (0.5 * rrf(1) + 0.5 * rrf(2))).abs() < 1e-9,
            "{a20:?}"
        );
        assert_eq!(a20.keyword_rank, Some(2));
        assert_eq!(a20.semantic_rank, Some(1));
        let a10 = by_id(10);
        assert!((a10.score - 0.5 * rrf(1)).abs() < 1e-9, "{a10:?}");
        let a40 = by_id(40);
        assert!((a40.score - 0.5 * rrf(2)).abs() < 1e-9, "{a40:?}");
        // 20 (両側) が最高、40 (意味2位) と 10 (KW1位) の比較:
        // 0.5*rrf(2)+0.5*rrf(2) vs 0.5*rrf(1) → 20 > (10 vs 40 は同値→id順)
        assert_eq!(fused[0].asset_id, 20);
        assert_eq!(fused[1].asset_id, 10, "同スコアならid昇順: {fused:?}");
        assert_eq!(fused[2].asset_id, 40);
        // 30 (KW3位) が最下位
        assert_eq!(fused[3].asset_id, 30);
        assert_eq!(fused.len(), 4, "和集合を返す");
    }

    #[test]
    fn weight_one_is_semantic_only() {
        let fused = weighted_rrf(&[10, 20], &[(30, 0.7), (20, 0.6)], 1.0);
        // w=1: キーワード項は0 — 30(意味1位) > 20(意味2位) > 10(スコア0)
        assert_eq!(fused[0].asset_id, 30);
        assert_eq!(fused[1].asset_id, 20);
        assert_eq!(fused[2].asset_id, 10);
        assert!((fused[0].score - rrf(1)).abs() < 1e-9);
        assert!(fused[2].score.abs() < 1e-9, "キーワードのみの項は0");
    }

    #[test]
    fn weight_zero_is_keyword_only() {
        let fused = weighted_rrf(&[10, 20], &[(30, 0.7)], 0.0);
        assert_eq!(fused[0].asset_id, 10);
        assert_eq!(fused[1].asset_id, 20);
        // 意味のみの30はスコア0で末尾 (呼び出し側で候補から外す設計)
        assert_eq!(fused[2].asset_id, 30);
        assert!(fused[2].score.abs() < 1e-9);
    }

    #[test]
    fn weight_ordering_shifts_with_weight() {
        // 意味1位 vs キーワード3位: wを上げると意味側が逆転する
        let low = weighted_rrf(&[1, 2, 3], &[(4, 0.9)], 0.1);
        let high = weighted_rrf(&[1, 2, 3], &[(4, 0.9)], 0.9);
        assert_eq!(low[0].asset_id, 1, "w小: KW1位が先頭: {low:?}");
        assert_eq!(high[0].asset_id, 4, "w大: 意味1位が先頭: {high:?}");
    }

    #[test]
    fn out_of_range_weight_is_clamped() {
        assert_eq!(clamp_weight(1.5), 1.0);
        assert_eq!(clamp_weight(-0.2), 0.0);
        let fused = weighted_rrf(&[1], &[], 7.0);
        assert!(fused[0].score.abs() < 1e-9, "w>1はw=1相当");
        let fused = weighted_rrf(&[1], &[], -1.0);
        assert!((fused[0].score - rrf(1)).abs() < 1e-9, "w<0はw=0相当");
    }

    #[test]
    fn deterministic_order_for_identical_scores() {
        // 同一キーワード順位構造で同点 → id昇順で安定
        let fused = weighted_rrf(&[7, 3, 11], &[], 0.5);
        let ids: Vec<i64> = fused.iter().map(|f| f.asset_id).collect();
        // スコアは rrf(1) > rrf(2) > rrf(3) のため入力順のまま
        assert_eq!(ids, vec![7, 3, 11]);
        // 逆順入力でも順位スコアが変わるだけで決定的
        let fused = weighted_rrf(&[11, 3, 7], &[], 0.5);
        let ids: Vec<i64> = fused.iter().map(|f| f.asset_id).collect();
        assert_eq!(ids, vec![11, 3, 7]);
    }

    #[test]
    fn empty_inputs_yield_empty() {
        assert!(weighted_rrf(&[], &[], 0.5).is_empty());
        assert!(weighted_rrf(&[], &[(1, 0.5)], 0.0).len() == 1);
    }

    #[test]
    fn duplicates_in_keyword_list_do_not_double_count_semantics() {
        // キーワード側に重複idが混入した場合も最初の出現を採用 (呼び出し側
        // は重複を除去する前提だが、混入時も爆発しない)
        let fused = weighted_rrf(&[5, 5], &[(5, 0.9)], 0.5);
        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].keyword_rank, Some(1));
        assert_eq!(fused[0].semantic_rank, Some(1));
    }
}
