import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PerfMetrics } from "./perf";

describe("PerfMetrics — 起動タイムマーク (WP-7)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("mount→health→初回ページの所要時間を導出する", () => {
    let t = 100;
    const perf = new PerfMetrics({ now: () => t });
    perf.markMounted();
    t = 180;
    perf.markHealthOk();
    t = 420;
    perf.markFirstPage();
    const durations = perf.startupDurations();
    expect(durations.mountToHealthMs).toBe(80);
    expect(durations.mountToFirstPageMs).toBe(320);
  });

  it("未計測の区間はnull (マークは冪等で最初の1回だけ有効)", () => {
    let t = 0;
    const perf = new PerfMetrics({ now: () => t });
    expect(perf.startupDurations()).toEqual({
      mountToHealthMs: null,
      mountToFirstPageMs: null,
    });
    perf.markMounted();
    t = 50;
    perf.markMounted(); // 2回目は無視
    perf.markHealthOk();
    expect(perf.startupDurations().mountToHealthMs).toBe(50);
    expect(perf.startupDurations().mountToFirstPageMs).toBeNull();
  });

  it("ロングタスクの件数と最悪値を記録し、カウンタだけリセットできる", () => {
    const perf = new PerfMetrics();
    perf.markMounted();
    perf.noteLongTask(62);
    perf.noteLongTask(120);
    perf.noteLongTask(51);
    expect(perf.longTasks).toBe(3);
    expect(perf.worstLongTaskMs).toBe(120);
    perf.resetCounters();
    expect(perf.longTasks).toBe(0);
    expect(perf.worstLongTaskMs).toBeNull();
    expect(perf.marks.mountedAt).not.toBeNull(); // マークは保持
  });
});

describe("PerfMetrics — フレーム監視", () => {
  it("閾値を超えたフレーム間隔をロングフレームとして数える", async () => {
    let t = 0;
    const perf = new PerfMetrics({
      now: () => t,
      frameThresholdMs: 50,
    });
    // requestAnimationFrameを即時実行の疑似実装に差し替える
    const raf = (cb: FrameRequestCallback): ReturnType<typeof requestAnimationFrame> => {
      queueMicrotask(() => cb(t));
      return 1 as ReturnType<typeof requestAnimationFrame>;
    };
    vi.stubGlobal("requestAnimationFrame", raf);
    vi.stubGlobal("cancelAnimationFrame", () => {});
    try {
      perf.startFrameMonitor();
      await Promise.resolve(); // 1フレーム目 (lastFrameAt設定)
      t += 16;
      await Promise.resolve(); // 通常フレーム
      t += 90;
      await Promise.resolve(); // ロングフレーム
      t += 200;
      await Promise.resolve(); // さらに長いフレーム
      expect(perf.longFrames).toBe(2);
      expect(perf.worstLongFrameMs).toBe(200);
      perf.stop();
    } finally {
      vi.unstubAllGlobals();
    }
  });
});
