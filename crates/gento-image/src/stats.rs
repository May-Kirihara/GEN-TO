//! サムネイル生成の統計 (Phase 4 / WP-3)。
//!
//! Atomicカウンタ + 生成時間の簡易ヒストグラム (p50/p95/p99を算出可能な
//! 生の時間列)。acceptance「生成時間・容量・キャッシュヒット率を記録できる」。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// ワーカー間で共有する統計カウンタ。
#[derive(Debug, Default)]
pub struct ThumbStats {
    /// 生成に成功した件数 (キャッシュ書き込み完了)。
    generated: AtomicU64,
    cache_hits: AtomicU64,
    cache_misses: AtomicU64,
    failures: AtomicU64,
    /// 退避 (LRU削除) したファイル数。
    evictions: AtomicU64,
    bytes_written: AtomicU64,
    bytes_evicted: AtomicU64,
    /// 生成時間 (ミリ秒)。get時にpercentile計算するため生値を保持する。
    durations_ms: Mutex<Vec<f64>>,
}

/// [`ThumbStats`] のスナップショット (IPC DTOの素材)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThumbStatsSnapshot {
    pub generated: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    /// hits / (hits + misses)。分母0 (データなし) の場合は None。
    pub hit_rate: Option<f64>,
    pub failures: u64,
    pub evictions: u64,
    pub bytes_written: u64,
    pub bytes_evicted: u64,
    pub p50_ms: Option<f64>,
    pub p95_ms: Option<f64>,
    pub p99_ms: Option<f64>,
}

impl ThumbStats {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_generated(&self, elapsed_ms: f64, bytes: u64) {
        self.generated.fetch_add(1, Ordering::Relaxed);
        self.bytes_written.fetch_add(bytes, Ordering::Relaxed);
        if let Ok(mut durations) = self.durations_ms.lock() {
            durations.push(elapsed_ms);
        }
    }

    pub fn record_hit(&self) {
        self.cache_hits.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_miss(&self) {
        self.cache_misses.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_failure(&self) {
        self.failures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_eviction(&self, bytes: u64) {
        self.evictions.fetch_add(1, Ordering::Relaxed);
        self.bytes_evicted.fetch_add(bytes, Ordering::Relaxed);
    }

    /// カウンタとpercentileのスナップショット。
    pub fn snapshot(&self) -> ThumbStatsSnapshot {
        let durations = self
            .durations_ms
            .lock()
            .map(|d| d.clone())
            .unwrap_or_default();
        let generated = self.generated.load(Ordering::Relaxed);
        let hits = self.cache_hits.load(Ordering::Relaxed);
        let misses = self.cache_misses.load(Ordering::Relaxed);
        ThumbStatsSnapshot {
            generated,
            cache_hits: hits,
            cache_misses: misses,
            hit_rate: hit_rate(hits, hits + misses),
            failures: self.failures.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
            bytes_written: self.bytes_written.load(Ordering::Relaxed),
            bytes_evicted: self.bytes_evicted.load(Ordering::Relaxed),
            p50_ms: percentile(&durations, 0.50),
            p95_ms: percentile(&durations, 0.95),
            p99_ms: percentile(&durations, 0.99),
        }
    }
}

/// ヒット率。hits + misses == 0 (データなし) は None。
pub fn hit_rate(hits: u64, total: u64) -> Option<f64> {
    if total == 0 {
        None
    } else {
        Some(hits as f64 / total as f64)
    }
}

/// nearest-rank法のpercentile。空ならNone。
pub fn percentile(sorted_input: &[f64], p: f64) -> Option<f64> {
    if sorted_input.is_empty() {
        return None;
    }
    let mut sorted = sorted_input.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank = (p * sorted.len() as f64).ceil();
    // rankは1-based。clampして境界誤差 (浮動小数) に耐える。
    let index = (rank as usize).clamp(1, sorted.len()) - 1;
    sorted.get(index).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_accumulate() {
        let stats = ThumbStats::new();
        stats.record_generated(10.0, 100);
        stats.record_generated(20.0, 200);
        stats.record_hit();
        stats.record_hit();
        stats.record_miss();
        stats.record_failure();
        stats.record_eviction(50);

        let snap = stats.snapshot();
        assert_eq!(snap.generated, 2);
        assert_eq!(snap.cache_hits, 2);
        assert_eq!(snap.cache_misses, 1);
        assert_eq!(snap.failures, 1);
        assert_eq!(snap.evictions, 1);
        assert_eq!(snap.bytes_written, 300);
        assert_eq!(snap.bytes_evicted, 50);
    }

    #[test]
    fn hit_rate_calculation() {
        assert_eq!(hit_rate(0, 0), None);
        let stats = ThumbStats::new();
        stats.record_hit();
        stats.record_hit();
        stats.record_miss();
        stats.record_miss();
        let snap = stats.snapshot();
        let rate = snap.hit_rate.expect("denominator > 0");
        assert!((rate - 0.5).abs() < 1e-9);
    }

    #[test]
    fn percentiles_known_values() {
        let v: Vec<f64> = (1..=100).map(|i| i as f64).collect();
        assert_eq!(percentile(&v, 0.50), Some(50.0));
        assert_eq!(percentile(&v, 0.95), Some(95.0));
        assert_eq!(percentile(&v, 0.99), Some(99.0));
        assert_eq!(percentile(&v, 1.0), Some(100.0));
        assert_eq!(percentile(&v, 0.0), Some(1.0));
    }

    #[test]
    fn percentiles_small_and_unsorted_inputs() {
        // 3件: p50はnearest-rankで2番目
        assert_eq!(percentile(&[3.0, 1.0, 2.0], 0.50), Some(2.0));
        // 1件はすべて同値
        assert_eq!(percentile(&[7.5], 0.95), Some(7.5));
        assert_eq!(percentile(&[], 0.5), None);
    }

    #[test]
    fn snapshot_percentiles_reflect_recorded_durations() {
        let stats = ThumbStats::new();
        for i in 1..=10 {
            stats.record_generated(i as f64, 1);
        }
        let snap = stats.snapshot();
        assert_eq!(snap.p50_ms, Some(5.0));
        assert_eq!(snap.p95_ms, Some(10.0));
        assert_eq!(snap.p99_ms, Some(10.0));
    }

    #[test]
    fn snapshot_without_data_has_none_percentiles_and_hit_rate() {
        let snap = ThumbStats::new().snapshot();
        assert_eq!(snap.hit_rate, None);
        assert_eq!(snap.p50_ms, None);
        assert_eq!(snap.p95_ms, None);
        assert_eq!(snap.p99_ms, None);
    }
}
