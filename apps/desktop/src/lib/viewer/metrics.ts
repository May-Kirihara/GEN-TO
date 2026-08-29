/**
 * 計測ユーティリティ (WP-6 / §13「表示準備時間を計測」、§14「フレーム時間を計測」)。
 *
 * percentileは近接順位法 (nearest-rank): 昇順ソート後 ceil(q*n) 番目。
 * Rust側 gento_image::stats::percentile と同じ方式で比較可能。
 */

/** 近接順位法のパーセンタイル。空はnull。入力は破壊しない。 */
export function percentile(values: readonly number[], q: number): number | null {
  if (values.length === 0) {
    return null;
  }
  const sorted = [...values].sort((a, b) => a - b);
  const rank = Math.min(sorted.length, Math.max(1, Math.ceil(q * sorted.length)));
  return sorted[rank - 1];
}

export type LatencyStats = {
  count: number;
  p50: number | null;
  p95: number | null;
  p99: number | null;
};

/** ナビゲーション→onload等の遅延サンプル (容量付き、古い方から破棄)。 */
export class LatencyLog {
  private samples: number[] = [];

  constructor(private readonly cap = 500) {}

  add(ms: number): void {
    this.samples.push(ms);
    if (this.samples.length > this.cap) {
      this.samples.splice(0, this.samples.length - this.cap);
    }
  }

  reset(): void {
    this.samples = [];
  }

  stats(): LatencyStats {
    return {
      count: this.samples.length,
      p50: percentile(this.samples, 0.5),
      p95: percentile(this.samples, 0.95),
      p99: percentile(this.samples, 0.99),
    };
  }
}

export type FrameStats = {
  count: number;
  p50: number | null;
  p95: number | null;
  max: number | null;
};

/** 同期パン中のrAFフレーム時間 (delta ms) の集計。 */
export class FrameLog {
  private samples: number[] = [];
  private recording = false;
  private last = 0;

  constructor(private readonly cap = 1000) {}

  /** 録画開始。nowは基準タイムスタンプ (ms)。 */
  start(now: number): void {
    this.recording = true;
    this.last = now;
  }

  /** rAFごとの呼び出し。録画中のみ記録する。 */
  tick(now: number): void {
    if (!this.recording) {
      return;
    }
    this.samples.push(now - this.last);
    this.last = now;
    if (this.samples.length > this.cap) {
      this.samples.splice(0, this.samples.length - this.cap);
    }
  }

  stop(): void {
    this.recording = false;
  }

  reset(): void {
    this.samples = [];
    this.recording = false;
  }

  stats(): FrameStats {
    return {
      count: this.samples.length,
      p50: percentile(this.samples, 0.5),
      p95: percentile(this.samples, 0.95),
      max: this.samples.length > 0 ? Math.max(...this.samples) : null,
    };
  }
}
