import { describe, expect, it } from "vitest";
import { FrameLog, LatencyLog, percentile } from "./metrics";

describe("percentile", () => {
  it("近接順位法 (nearest-rank) で計算する", () => {
    // 1..100: p50=50, p95=95, p99=99
    const values = Array.from({ length: 100 }, (_, i) => i + 1);
    expect(percentile(values, 0.5)).toBe(50);
    expect(percentile(values, 0.95)).toBe(95);
    expect(percentile(values, 0.99)).toBe(99);
  });

  it("少数サンプルでも壊れない (ceil(q*n) ≥ 1)", () => {
    expect(percentile([42], 0.99)).toBe(42);
    expect(percentile([10, 20], 0.5)).toBe(10);
    expect(percentile([10, 20], 0.95)).toBe(20);
  });

  it("空はnull、入力を破壊しない", () => {
    expect(percentile([], 0.5)).toBeNull();
    const unsorted = [3, 1, 2];
    percentile(unsorted, 0.5);
    expect(unsorted).toEqual([3, 1, 2]);
  });
});

describe("LatencyLog", () => {
  it("サンプルを追加してp50/p95/p99を返す", () => {
    const log = new LatencyLog();
    expect(log.stats()).toEqual({ count: 0, p50: null, p95: null, p99: null });
    for (let i = 1; i <= 10; i++) {
      log.add(i * 10);
    }
    expect(log.stats()).toEqual({ count: 10, p50: 50, p95: 100, p99: 100 });
  });

  it("resetで空に戻る", () => {
    const log = new LatencyLog();
    log.add(5);
    log.add(10);
    log.reset();
    expect(log.stats().count).toBe(0);
  });

  it("上限を超えたサンプルは古い方から落ちる (容量付きリング)", () => {
    const log = new LatencyLog(5);
    for (let i = 1; i <= 8; i++) {
      log.add(i);
    }
    const stats = log.stats();
    expect(stats.count).toBe(5);
    expect(stats.p50).toBe(6); // 4..8 の中央
  });
});

describe("FrameLog", () => {
  it("フレーム時間deltaの p50/p95/max を集計する", () => {
    const log = new FrameLog();
    log.start(0);
    // delta: 16, 17, 15, 32, 16
    for (const ts of [16, 33, 48, 80, 96]) {
      log.tick(ts);
    }
    log.stop();
    const stats = log.stats();
    expect(stats.count).toBe(5);
    expect(stats.p50).toBe(16);
    expect(stats.p95).toBe(32);
    expect(stats.max).toBe(32);
  });

  it("recording中でないtickは無視、resetで初期化", () => {
    const log = new FrameLog();
    log.tick(100);
    expect(log.stats().count).toBe(0);
    log.start(0);
    log.tick(16);
    log.stop();
    log.reset();
    expect(log.stats()).toEqual({ count: 0, p50: null, p95: null, max: null });
  });
});
