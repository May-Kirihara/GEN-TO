import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { GridStore, type GridIpc } from "./grid-store";
import type { CatalogPage, CatalogSummary } from "../catalog-types";
import type { RequestAccepted, ThumbReadyItem } from "../thumbnail-types";

type Listener = (event: { payload: unknown }) => void;

function makeIpc(opts?: {
  pageTotal?: number;
  accepted?: Partial<RequestAccepted>;
}) {
  const listeners = new Map<string, Listener>();
  const calls: Array<{ command: string; args?: Record<string, unknown> }> = [];
  const total = opts?.pageTotal ?? 50_000;
  const ipc: GridIpc = {
    invoke: async <T>(command: string, args?: Record<string, unknown>): Promise<T> => {
      calls.push({ command, args });
      if (command === "get_catalog_summary") {
        return {
          generation: 1,
          mode: "virtual",
          total,
          scanning: false,
          scanned_count: 1615,
          error_count: 0,
          last_scan_elapsed_ms: 1,
        } as T;
      }
      if (command === "get_catalog_page") {
        const offset = args?.offset as number;
        const count = Math.min(200, total - offset);
        const items = Array.from({ length: Math.max(0, count) }, (_, i) => ({
          id: String(offset + i),
          displayName: `item-${offset + i}`,
          width: 100,
          height: 100,
          byteSize: 1000,
          state: "ready" as const,
          thumbnailState: "missing" as const,
          version: 800_000 + offset + i,
        }));
        return {
          generation: args?.generation,
          total,
          offset,
          items,
          has_more: offset + count < total,
        } as CatalogPage as T;
      }
      if (command === "request_thumbnails") {
        return {
          queued: (args?.ids as string[]).length,
          rejected_dup: 0,
          rejected_full: 0,
          cache_hits: 0,
          skipped_unreadable: 0,
          skipped_unknown: 0,
          ...opts?.accepted,
        } as RequestAccepted as T;
      }
      if (command === "cancel_thumbnail_requests") {
        const count = (args?.ids as string[]).length;
        return {
          cancelled_queued: count,
          cancel_requested: 0,
          not_found: 0,
          skipped_unreadable: 0,
        } as T;
      }
      throw `unexpected command ${command}`;
    },
    listen: async <T>(
      event: string,
      handler: (event: { payload: T }) => void,
    ): Promise<() => void> => {
      listeners.set(event, handler as Listener);
      return () => {
        listeners.delete(event);
      };
    },
  };
  return { ipc, calls, listeners };
}

function emit(listeners: Map<string, Listener>, event: string, payload: unknown): void {
  listeners.get(event)?.({ payload });
}

function readyItems(
  ids: string[],
  gen: number,
  sizeClass = "s256",
  version = gen,
): { items: ThumbReadyItem[] } {
  return {
    items: ids.map((id) => ({
      asset_id: id,
      size_class: sizeClass,
      cache_generation: gen,
      version,
    })),
  };
}

function requestCalls(calls: Array<{ command: string; args?: Record<string, unknown> }>) {
  return calls.filter((c) => c.command === "request_thumbnails");
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("GridStore — 需要スケジューリング", () => {
  it("可視範囲の未生成IDをデバウンス後にviewport優先度で要求する", async () => {
    const { ipc, calls } = makeIpc();
    const grid = new GridStore(ipc, { debounceMs: 150, prefetchRows: 0 });
    await grid.init();
    grid.viewportChanged({ start: 0, end: 2 }, 0, 0, 3);
    vi.advanceTimersByTime(50);
    expect(requestCalls(calls)).toHaveLength(0);
    await vi.advanceTimersByTimeAsync(150);
    const requests = requestCalls(calls);
    expect(requests).toHaveLength(1);
    expect(requests[0]?.args?.priority).toBe("viewport");
    expect(requests[0]?.args?.sizeClass).toBe("s256");
    expect(requests[0]?.args?.ids).toEqual(["0", "1", "2", "3", "4", "5", "6", "7", "8"]);
    expect(grid.stats.requested).toBe(9);
  });

  it("進行方向の先読み行をprefetch優先度で追加要求する", async () => {
    const { ipc, calls } = makeIpc();
    const grid = new GridStore(ipc, { debounceMs: 150, prefetchRows: 2 });
    await grid.init();
    grid.viewportChanged({ start: 0, end: 2 }, 0, 100, 3);
    grid.viewportChanged({ start: 0, end: 2 }, 0, 130, 3); // 下向き
    await vi.advanceTimersByTimeAsync(200);
    const requests = requestCalls(calls);
    expect(requests).toHaveLength(2);
    expect(requests[0]?.args?.priority).toBe("viewport");
    expect(requests[1]?.args?.priority).toBe("prefetch");
    expect(requests[1]?.args?.ids).toEqual(["9", "10", "11", "12", "13", "14"]);
  });

  it("デバウンス窓内の複数更新は最後の範囲で1回だけ要求する", async () => {
    const { ipc, calls } = makeIpc();
    const grid = new GridStore(ipc, { debounceMs: 150, prefetchRows: 0 });
    await grid.init();
    grid.viewportChanged({ start: 0, end: 2 }, 0, 0, 3);
    grid.viewportChanged({ start: 2, end: 4 }, 0, 10, 3);
    grid.viewportChanged({ start: 4, end: 6 }, 0, 30, 3);
    await vi.advanceTimersByTimeAsync(200);
    const requests = requestCalls(calls);
    expect(requests).toHaveLength(1);
    expect(requests[0]?.args?.ids).toEqual(["12", "13", "14", "15", "16", "17", "18", "19", "20"]);
  });

  it("要求済み (pending) のIDは再要求しない", async () => {
    const { ipc, calls } = makeIpc();
    const grid = new GridStore(ipc, { debounceMs: 150, prefetchRows: 0 });
    await grid.init();
    grid.viewportChanged({ start: 0, end: 2 }, 0, 0, 3);
    await vi.advanceTimersByTimeAsync(200);
    grid.viewportChanged({ start: 0, end: 2 }, 0, 0, 3);
    await vi.advanceTimersByTimeAsync(200);
    expect(requestCalls(calls)).toHaveLength(1);
  });

  it("thumb:readyでpendingが解放されURLを構成できる", async () => {
    const { ipc, calls, listeners } = makeIpc();
    const grid = new GridStore(ipc, { debounceMs: 150, prefetchRows: 0 });
    await grid.init();
    expect(grid.thumbUrlFor("3")).toBeNull();
    grid.viewportChanged({ start: 0, end: 2 }, 0, 0, 3);
    await vi.advanceTimersByTimeAsync(200);
    emit(listeners, "thumb:ready", readyItems(["3"], 1));
    expect(grid.stats.readyReceived).toBe(1);
    expect(grid.thumbUrlFor("3")).toBe("gento-thumb://localhost/asset/3/s256?v=1");
    expect(grid.thumbUrlFor("4")).toBeNull();
    // ready済み・pending残りを除外すると空になる → 追加要求は発行しない
    grid.viewportChanged({ start: 0, end: 2 }, 0, 0, 3);
    await vi.advanceTimersByTimeAsync(200);
    expect(requestCalls(calls)).toHaveLength(1);
  });

  it("thumb:failedは失敗地図へ記録し再要求対象から除外する", async () => {
    const { ipc, calls, listeners } = makeIpc();
    const grid = new GridStore(ipc, { debounceMs: 150, prefetchRows: 0 });
    await grid.init();
    grid.viewportChanged({ start: 0, end: 2 }, 0, 0, 3);
    await vi.advanceTimersByTimeAsync(200);
    emit(listeners, "thumb:failed", {
      items: [{ asset_id: "5", size_class: "s256", reason: "decode" }],
    });
    expect(grid.stats.failedReceived).toBe(1);
    // readyで全件解放してから再計画 → 5だけ除外される
    emit(listeners, "thumb:ready", readyItems(["0", "1", "2", "3", "4", "6", "7", "8"], 1));
    grid.viewportChanged({ start: 0, end: 2 }, 0, 0, 3);
    await vi.advanceTimersByTimeAsync(200);
    expect(requestCalls(calls)).toHaveLength(1);
  });

  it("rejected_fullは統計に計上し次のレンジ変化で再計画される", async () => {
    const { ipc, calls } = makeIpc({ accepted: { rejected_full: 9, queued: 0 } });
    const grid = new GridStore(ipc, { debounceMs: 150, prefetchRows: 0 });
    await grid.init();
    grid.viewportChanged({ start: 0, end: 2 }, 0, 0, 3);
    await vi.advanceTimersByTimeAsync(200);
    expect(grid.stats.rejectedFull).toBe(9);
    // pendingから除外されるため同範囲でも再要求する
    grid.viewportChanged({ start: 0, end: 2 }, 0, 0, 3);
    await vi.advanceTimersByTimeAsync(200);
    expect(requestCalls(calls)).toHaveLength(2);
  });

  it("サイズクラス切替で未生成の同じIDを再要求する", async () => {
    const { ipc, calls } = makeIpc();
    const grid = new GridStore(ipc, { debounceMs: 150, prefetchRows: 0 });
    await grid.init();
    grid.viewportChanged({ start: 0, end: 2 }, 0, 0, 3);
    await vi.advanceTimersByTimeAsync(200);
    grid.setSizeClass("s512");
    grid.viewportChanged({ start: 0, end: 2 }, 0, 0, 3);
    await vi.advanceTimersByTimeAsync(200);
    const requests = requestCalls(calls);
    expect(requests[1]?.args?.sizeClass).toBe("s512");
    expect(requests[1]?.args?.ids).toEqual(["0", "1", "2", "3", "4", "5", "6", "7", "8"]);
  });

  it("深い行位置では行×列で変換したindexのページを取得し需要を発行する (回帰: 行番号をindexと誤解しない)", async () => {
    const { ipc, calls } = makeIpc();
    const grid = new GridStore(ipc, { debounceMs: 150, prefetchRows: 0 });
    await grid.init();
    // 行76..80 × 7列 → index 532..560 → ページ2 (offset 400) が必要
    grid.viewportChanged({ start: 76, end: 80 }, 0, 0, 7);
    await vi.advanceTimersByTimeAsync(200);
    const offsets = calls
      .filter((c) => c.command === "get_catalog_page")
      .map((c) => c.args?.offset as number);
    expect(offsets).toContain(400);
    const requests = requestCalls(calls);
    const ids = requests[0]?.args?.ids as string[];
    expect(ids[0]).toBe("532");
    expect(ids.at(-1)).toBe("566"); // 行80 = index 560..566
    expect(grid.getItem(532)?.id).toBe("532");
  });

  it("request_thumbnails応答喪失はタイムアウトでpendingを解放し再計画できる", async () => {
    const listeners = new Map<string, Listener>();
    const calls: Array<{ command: string; args?: Record<string, unknown> }> = [];
    const total = 50_000;
    const lost = true;
    const ipc: GridIpc = {
      invoke: <T>(command: string, args?: Record<string, unknown>): Promise<T> => {
        calls.push({ command, args });
        if (command === "request_thumbnails" && lost) {
          return new Promise<T>(() => {});
        }
        if (command === "get_catalog_summary") {
          return Promise.resolve({
            generation: 1,
            mode: "virtual",
            total,
            scanning: false,
            scanned_count: 1615,
            error_count: 0,
            last_scan_elapsed_ms: 1,
          } as T);
        }
        if (command === "get_catalog_page") {
          const offset = args?.offset as number;
          return Promise.resolve(
            {
              generation: 1,
              total,
              offset,
              has_more: true,
              items: Array.from({ length: 200 }, (_, i) => ({
                id: String(offset + i),
                displayName: `item-${offset + i}`,
                width: 100,
                height: 100,
                byteSize: 1000,
                state: "ready" as const,
                thumbnailState: "missing" as const,
                version: 800_000 + offset + i,
              })),
            } as CatalogPage as T,
          );
        }
        return Promise.resolve({ queued: 9, rejected_dup: 0, rejected_full: 0, cache_hits: 0, skipped_unreadable: 0, skipped_unknown: 0 } as T);
      },
      listen: async <T>(
        event: string,
        handler: (event: { payload: T }) => void,
      ): Promise<() => void> => {
        listeners.set(event, handler as Listener);
        return () => {
          listeners.delete(event);
        };
      },
    };
    const grid = new GridStore(ipc, { debounceMs: 150, prefetchRows: 0, requestTimeoutMs: 100 });
    await grid.init();
    grid.viewportChanged({ start: 0, end: 2 }, 0, 0, 3);
    await vi.advanceTimersByTimeAsync(200);
    expect(requestCalls(calls)).toHaveLength(1);
    // pendingに残ったまま (応答なし)
    grid.viewportChanged({ start: 0, end: 2 }, 0, 0, 3);
    await vi.advanceTimersByTimeAsync(50);
    expect(requestCalls(calls)).toHaveLength(1); // pendingのため再要求しない
    await vi.advanceTimersByTimeAsync(100); // タイムアウト発火
    expect(grid.lastError).toContain("timed out");
    grid.viewportChanged({ start: 0, end: 2 }, 0, 0, 3);
    await vi.advanceTimersByTimeAsync(200);
    // pending解放後は再度要求できる (喪失時より回数が増える)
    expect(requestCalls(calls).length).toBeGreaterThanOrEqual(2);
    expect(grid.stats.rejectedFull).toBe(0);
  });
});

describe("GridStore — 世代ガード", () => {
  it("カタログgeneration変化でready地図とpendingを初期化する", async () => {
    const { ipc, listeners } = makeIpc();
    const grid = new GridStore(ipc, { debounceMs: 150 });
    await grid.init();
    grid.viewportChanged({ start: 0, end: 2 }, 0, 0, 3);
    await vi.advanceTimersByTimeAsync(200);
    emit(listeners, "thumb:ready", readyItems(["3"], 1));
    expect(grid.thumbUrlFor("3")).toContain("v=1");
    grid.syncSummary({
      generation: 2,
      mode: "virtual",
      total: 50_000,
      scanning: false,
      scanned_count: 1615,
      error_count: 0,
      last_scan_elapsed_ms: 1,
    });
    expect(grid.thumbUrlFor("3")).toBeNull();
  });

  it("キャッシュクリア後の古い世代readyイベントは破棄される", async () => {
    const { ipc, listeners } = makeIpc();
    const grid = new GridStore(ipc, { debounceMs: 150 });
    await grid.init();
    grid.noteCacheCleared(2);
    emit(listeners, "thumb:ready", readyItems(["3"], 1));
    expect(grid.thumbUrlFor("3")).toBeNull();
    emit(listeners, "thumb:ready", readyItems(["3"], 2));
    expect(grid.thumbUrlFor("3")).toBe("gento-thumb://localhost/asset/3/s256?v=2");
  });

  it("URLのvは内容感応versionを使い (generationとは独立)、世代ガードはgenerationで効く", async () => {
    const { ipc, listeners } = makeIpc();
    const grid = new GridStore(ipc, { debounceMs: 150 });
    await grid.init();
    // generation=2・version=999999 のready → URLの ?v= は999999
    emit(listeners, "thumb:ready", readyItems(["3"], 2, "s256", 999_999));
    expect(grid.thumbUrlFor("3")).toBe("gento-thumb://localhost/asset/3/s256?v=999999");
    // クリア (generation 3) で古い世代 (2) の項目は一掃される
    grid.noteCacheCleared(3);
    emit(listeners, "thumb:ready", readyItems(["3"], 2, "s256", 999_999));
    expect(grid.thumbUrlFor("3")).toBeNull();
  });
});

describe("GridStore — summary同期", () => {
  it("summaryのtotalとmodeを公開する", async () => {
    const { ipc } = makeIpc();
    const grid = new GridStore(ipc);
    await grid.init();
    expect(grid.summary?.total).toBe(50_000);
    expect(grid.summary?.mode).toBe("virtual");
    const summary: CatalogSummary = {
      generation: 1,
      mode: "real",
      total: 1615,
      scanning: true,
      scanned_count: 10,
      error_count: 0,
      last_scan_elapsed_ms: null,
    };
    grid.syncSummary(summary);
    expect(grid.summary?.total).toBe(1615);
  });
});

describe("GridStore — 画面外キャンセルと失敗リトラクト (WP-7)", () => {
  function cancelCalls(calls: Array<{ command: string; args?: Record<string, unknown> }>) {
    return calls.filter((c) => c.command === "cancel_thumbnail_requests");
  }

  it("スクロールで画面外へ出たpending要求をキャンセルし、進行方向先読みは維持する", async () => {
    const { ipc, calls } = makeIpc();
    const grid = new GridStore(ipc, { debounceMs: 150, prefetchRows: 0 });
    await grid.init();
    // 行0..2 × 3列 = index 0..8 を要求 → pendingに入る
    grid.viewportChanged({ start: 0, end: 2 }, 0, 0, 3);
    await vi.advanceTimersByTimeAsync(200);
    expect(requestCalls(calls)).toHaveLength(1);

    // 行20..22へ一気にスクロール (下向き) → 0..8は画面外 → キャンセルされる
    grid.viewportChanged({ start: 20, end: 22 }, 0, 5000, 3);
    await vi.advanceTimersByTimeAsync(200);
    const cancels = cancelCalls(calls);
    expect(cancels).toHaveLength(1);
    expect(cancels[0]?.args?.sizeClass).toBe("s256");
    expect((cancels[0]?.args?.ids as string[]).sort()).toEqual(
      ["0", "1", "2", "3", "4", "5", "6", "7", "8"].sort(),
    );
    expect(grid.stats.cancelled).toBe(9);
    // pendingから除去されるため、行0..2へ戻れば再要求できる
    grid.viewportChanged({ start: 0, end: 2 }, 0, 0, 3);
    await vi.advanceTimersByTimeAsync(200);
    // 2回目 (行20..22の要求) + 3回目 (行0..2の再要求)
    expect(requestCalls(calls)).toHaveLength(3);
  });

  it("先読み・overscan行はキャンセルしない (維持ゾーン内)", async () => {
    const { ipc, calls } = makeIpc();
    const grid = new GridStore(ipc, { debounceMs: 150, prefetchRows: 4 });
    await grid.init();
    grid.viewportChanged({ start: 0, end: 2 }, 2, 0, 3);
    await vi.advanceTimersByTimeAsync(200);
    // 下向きに2行だけスクロール (overscan 2 + prefetch 4が旧要求をカバーする)
    grid.viewportChanged({ start: 2, end: 4 }, 2, 100, 3);
    await vi.advanceTimersByTimeAsync(200);
    expect(cancelCalls(calls)).toHaveLength(0);
  });

  it("現在以外のサイズクラスのpendingはキャンセル対象にしない", async () => {
    const { ipc, calls } = makeIpc();
    const grid = new GridStore(ipc, { debounceMs: 150, prefetchRows: 0 });
    await grid.init();
    grid.viewportChanged({ start: 0, end: 2 }, 0, 0, 3);
    await vi.advanceTimersByTimeAsync(200);
    // s512へ切替して要求 → s512のpendingが増える
    grid.setSizeClass("s512");
    grid.viewportChanged({ start: 0, end: 2 }, 0, 0, 3);
    await vi.advanceTimersByTimeAsync(200);
    // 大きくスクロール: s512のpendingはキャンセルされるが、
    // キャンセル要求のsizeClassは現在のクラス (s512) に限定される
    grid.viewportChanged({ start: 20, end: 22 }, 0, 5000, 3);
    await vi.advanceTimersByTimeAsync(200);
    const cancels = cancelCalls(calls);
    expect(cancels).toHaveLength(1);
    expect(cancels[0]?.args?.sizeClass).toBe("s512");
  });

  it("画面外へ出た失敗記録はリトラクトされ、再登場時に再試行する", async () => {
    const { ipc, calls, listeners } = makeIpc();
    const grid = new GridStore(ipc, { debounceMs: 150, prefetchRows: 0 });
    await grid.init();
    grid.viewportChanged({ start: 0, end: 2 }, 0, 0, 3);
    await vi.advanceTimersByTimeAsync(200);
    emit(listeners, "thumb:failed", {
      items: [{ asset_id: "5", size_class: "s256", reason: "decode failed: injected" }],
    });
    expect(grid.isFailed("5")).toBe(true);

    // 画面外へ出る → 失敗記録がリトラクトされる
    grid.viewportChanged({ start: 20, end: 22 }, 0, 5000, 3);
    await vi.advanceTimersByTimeAsync(200);
    expect(grid.isFailed("5")).toBe(false);

    // 戻ってくると再要求される (失敗記録が解除されているため)
    emit(listeners, "thumb:ready", readyItems(["0", "1", "2", "3", "4", "6", "7", "8"], 1));
    grid.viewportChanged({ start: 0, end: 2 }, 0, 0, 3);
    await vi.advanceTimersByTimeAsync(200);
    const last = requestCalls(calls).at(-1);
    expect(last?.args?.ids).toContain("5");
  });

  it("キャンセル要求の結果は統計に計上される", async () => {
    const { ipc } = makeIpc();
    const grid = new GridStore(ipc, { debounceMs: 150, prefetchRows: 0 });
    await grid.init();
    grid.viewportChanged({ start: 0, end: 2 }, 0, 0, 3);
    await vi.advanceTimersByTimeAsync(200);
    grid.viewportChanged({ start: 40, end: 42 }, 0, 9000, 3);
    await vi.advanceTimersByTimeAsync(200);
    expect(grid.stats.cancelled).toBe(9);
    expect(grid.stats.cancelBatches).toBe(1);
  });
});
