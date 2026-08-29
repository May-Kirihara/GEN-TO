import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  EVENT_LOG_MAX,
  ThumbnailStore,
  formatBytes,
  formatHitRate,
  formatMs,
  type ThumbnailIpc,
  type ThumbnailStats,
} from "./thumbnails";

type InvokeCall = { command: string; args?: Record<string, unknown> };

class MockIpc implements ThumbnailIpc {
  readonly calls: InvokeCall[] = [];
  private readonly responses = new Map<string, unknown>();
  private readonly errors = new Map<string, unknown>();
  private readonly listeners = new Map<string, (event: { payload: unknown }) => void>();

  enqueue(command: string, response: unknown): void {
    this.responses.set(command, response);
  }

  failWith(command: string, reason: unknown): void {
    this.errors.set(command, reason);
  }

  async invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
    this.calls.push({ command, args });
    if (this.errors.has(command)) {
      throw this.errors.get(command);
    }
    const response = this.responses.get(command);
    if (response === undefined) {
      throw new Error(`no queued response for ${command}`);
    }
    return response as T;
  }

  async listen<T>(
    event: string,
    handler: (event: { payload: T }) => void,
  ): Promise<() => void> {
    this.listeners.set(event, handler as (event: { payload: unknown }) => void);
    return () => {
      this.listeners.delete(event);
    };
  }

  emit(event: string, payload: unknown): void {
    this.listeners.get(event)?.({ payload });
  }

  callsFor(command: string): InvokeCall[] {
    return this.calls.filter((c) => c.command === command);
  }
}

function stats(partial: Partial<ThumbnailStats> = {}): ThumbnailStats {
  return {
    generated: 10,
    hits: 4,
    misses: 6,
    hit_rate: 0.4,
    failures: 1,
    evictions: 2,
    queue_depth: 3,
    queue_depths: { interactive: 0, viewport: 2, prefetch: 1, overscan: 0, background: 0 },
    injection_mode: "none",
    injection_rate: 0,
    cache_bytes: 123456789,
    p50_ms: 12.5,
    p95_ms: 40.25,
    p99_ms: 90.125,
    paused: false,
    proto_requests: 0,
    proto_ok: 0,
    proto_bytes_sent: 0,
    proto_p50_ms: null,
    proto_p95_ms: null,
    proto_p99_ms: null,
    ...partial,
  };
}

describe("書式関数", () => {
  it("formatHitRate: nullは「—」、それ以外は百分率", () => {
    expect(formatHitRate(null)).toBe("—");
    expect(formatHitRate(0)).toBe("0.0%");
    expect(formatHitRate(0.4)).toBe("40.0%");
    expect(formatHitRate(1)).toBe("100.0%");
    expect(formatHitRate(0.1234)).toBe("12.3%");
  });

  it("formatMs: nullは「—」、1秒未満はms、以上は秒", () => {
    expect(formatMs(null)).toBe("—");
    expect(formatMs(12.4)).toBe("12 ms");
    expect(formatMs(0.4)).toBe("0 ms");
    expect(formatMs(1234)).toBe("1.23 s");
    expect(formatMs(60000)).toBe("60.00 s");
  });

  it("formatBytes: 単位の切り替え", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(2048)).toBe("2 KiB");
    expect(formatBytes(5 * 1024 * 1024)).toBe("5.0 MiB");
    expect(formatBytes(2 * 1024 * 1024 * 1024)).toBe("2.00 GiB");
  });
});

describe("ThumbnailStore", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  function makeStore(pollMs = 1000): { store: ThumbnailStore; ipc: MockIpc } {
    const ipc = new MockIpc();
    ipc.enqueue("get_thumbnail_stats", stats());
    const store = new ThumbnailStore(ipc, pollMs);
    return { store, ipc };
  }

  async function initStore(pollMs = 1000) {
    const ctx = makeStore(pollMs);
    // init内でrefreshStatsをawaitするため、戻り時点でstatsが入っている
    const dispose = await ctx.store.init();
    expect(ctx.store.stats).not.toBeNull();
    return { ...ctx, dispose };
  }

  it("initで統計を取得し1秒間隔でポーリングする", async () => {
    const { store, ipc, dispose } = await initStore();
    expect(store.stats).toEqual(stats());
    expect(ipc.callsFor("get_thumbnail_stats").length).toBe(1);

    await vi.advanceTimersByTimeAsync(1000);
    expect(ipc.callsFor("get_thumbnail_stats").length).toBe(2);
    await vi.advanceTimersByTimeAsync(3000);
    expect(ipc.callsFor("get_thumbnail_stats").length).toBe(5);

    dispose();
    await vi.advanceTimersByTimeAsync(5000);
    // dispose後は停止
    expect(ipc.callsFor("get_thumbnail_stats").length).toBe(5);
  });

  it("thumb:readyイベントでログとカウンタを更新", async () => {
    const { store, ipc, dispose } = await initStore();
    const before = store.eventLog.length;
    ipc.emit("thumb:ready", {
      items: [
        { asset_id: "7", size_class: "s256", cache_generation: 1 },
        { asset_id: "8", size_class: "s256", cache_generation: 1 },
      ],
    });
    expect(store.readyCount).toBe(2);
    expect(store.eventLog.length).toBe(before + 1);
    expect(store.eventLog[0]).toContain("ready: 2 件");
    expect(store.eventLog[0]).toContain("7");
    dispose();
  });

  it("thumb:failedイベントで理由付きログを追加", async () => {
    const { store, ipc, dispose } = await initStore();
    ipc.emit("thumb:failed", {
      items: [{ asset_id: "9", size_class: "s256", reason: "decode failed: bad png" }],
    });
    expect(store.failedCount).toBe(1);
    expect(store.eventLog[0]).toContain("failed: 9");
    expect(store.eventLog[0]).toContain("decode failed");
    dispose();
  });

  it("イベントログは上限で打ち切る", async () => {
    const { store, ipc, dispose } = await initStore();
    for (let i = 0; i < EVENT_LOG_MAX + 10; i += 1) {
      ipc.emit("thumb:ready", {
        items: [{ asset_id: String(i), size_class: "s256", cache_generation: 1 }],
      });
    }
    expect(store.eventLog.length).toBe(EVENT_LOG_MAX);
    dispose();
  });

  it("generateAllはsizeClass付きでcommandを呼ぶ", async () => {
    const { store, ipc, dispose } = await initStore();
    ipc.enqueue("generate_all_thumbnails", {
      queued: 1615,
      rejected_dup: 0,
      rejected_full: 0,
      cache_hits: 0,
      skipped_unreadable: 0,
      skipped_unknown: 0,
    });
    const ok = await store.generateAll("s256");
    expect(ok).toBe(true);
    expect(ipc.callsFor("generate_all_thumbnails")).toEqual([
      { command: "generate_all_thumbnails", args: { sizeClass: "s256" } },
    ]);
    expect(store.lastNotice).toContain("1615");
    dispose();
  });

  it("setPausedとclearCacheの呼び出し", async () => {
    const { store, ipc, dispose } = await initStore();
    await store.setPaused(true);
    expect(ipc.callsFor("pause_thumbnail_generation")).toEqual([
      { command: "pause_thumbnail_generation", args: { paused: true } },
    ]);

    ipc.enqueue("clear_thumbnail_cache", {
      removed_files: 3,
      removed_bytes: 4096,
      cache_generation: 2,
    });
    await store.clearCache();
    expect(ipc.callsFor("clear_thumbnail_cache").length).toBe(1);
    expect(store.lastNotice).toContain("3 件");
    dispose();
  });

  it("command失敗はlastErrorへ", async () => {
    const { store, ipc, dispose } = await initStore();
    ipc.failWith("generate_all_thumbnails", "unknown size class: expected one of s256|s512|preview2048");
    const ok = await store.generateAll("x256");
    expect(ok).toBe(false);
    expect(store.lastError).toContain("unknown size class");
    dispose();
  });

  it("統計取得失敗でもポーリングは継続", async () => {
    const { store, ipc, dispose } = await initStore();
    ipc.failWith("get_thumbnail_stats", "boom");
    await vi.advanceTimersByTimeAsync(1000);
    expect(store.lastError).toBe("boom");
    // 失敗後も次のtickで再試行する
    await vi.advanceTimersByTimeAsync(1000);
    expect(ipc.callsFor("get_thumbnail_stats").length).toBeGreaterThanOrEqual(3);
    dispose();
  });
});
