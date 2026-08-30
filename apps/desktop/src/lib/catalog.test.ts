import { describe, expect, it, vi } from "vitest";
import { CatalogStore, type CatalogIpc, type CatalogPage, type CatalogSummary } from "./catalog";

type InvokeCall = { command: string; args?: Record<string, unknown> };

type QueueEntry = unknown | ((args: Record<string, unknown> | undefined) => unknown);

/** モックinvokeで拒否 (command error) を表すラッパー。 */
class Rejection {
  constructor(readonly reason: unknown) {}
}

function rejectWith(reason: unknown): Rejection {
  return new Rejection(reason);
}

class MockIpc implements CatalogIpc {
  readonly calls: InvokeCall[] = [];
  private readonly queues = new Map<string, QueueEntry[]>();
  private readonly listeners = new Map<string, (event: { payload: unknown }) => void>();

  enqueue(command: string, ...entries: QueueEntry[]): void {
    const queue = this.queries(command);
    queue.push(...entries);
  }

  async invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
    this.calls.push({ command, args });
    const entry = this.queues.get(command)?.shift();
    if (entry === undefined) {
      throw new Error(`unexpected invoke of ${command} with no queued response`);
    }
    if (entry instanceof Rejection) {
      throw entry.reason;
    }
    const value = typeof entry === "function" ? entry(args) : entry;
    if (value instanceof Error) {
      throw value;
    }
    return value as T;
  }

  async listen<T>(_event: string, handler: (event: { payload: T }) => void): Promise<() => void> {
    const name = _event;
    this.listeners.set(name, handler as (event: { payload: unknown }) => void);
    return () => {
      this.listeners.delete(name);
    };
  }

  emit(event: string, payload: unknown): void {
    this.listeners.get(event)?.({ payload });
  }

  callsFor(command: string): InvokeCall[] {
    return this.calls.filter((c) => c.command === command);
  }

  argsOf(command: string): Array<Record<string, unknown> | undefined> {
    return this.callsFor(command).map((c) => c.args);
  }

  private queries(command: string): QueueEntry[] {
    let queue = this.queues.get(command);
    if (!queue) {
      queue = [];
      this.queues.set(command, queue);
    }
    return queue;
  }
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function makeSummary(overrides: Partial<CatalogSummary> = {}): CatalogSummary {
  return {
    generation: 2,
    mode: "real",
    total: 1615,
    scanning: false,
    scanned_count: 1615,
    error_count: 0,
    last_scan_elapsed_ms: 4321,
    sort: "name",
    ...overrides,
  };
}

function makePage(generation: number, offset: number, names: string[]): CatalogPage {
  return {
    generation,
    total: 1615,
    offset,
    has_more: offset + names.length < 1615,
    items: names.map((name, i) => ({
      id: String(offset + i + 1),
      displayName: name,
      width: 3840,
      height: 2048,
      byteSize: 12345,
      state: "ready" as const,
      thumbnailState: "missing" as const,
      isAnimated: false,
      version: 900_000 + offset + i,
      path: `/dataset/${name}`,
      mtimeMs: 1_700_000_000_000,
      mimeType: "image/png",
      dbState: "adhoc" as const,
      triage: "undecided" as const,
    })),
  };
}

function setup(entries?: { summary?: QueueEntry[] }): { ipc: MockIpc; store: CatalogStore } {
  const ipc = new MockIpc();
  if (entries?.summary) {
    ipc.enqueue("get_catalog_summary", ...entries.summary);
  }
  const store = new CatalogStore(ipc, 200);
  return { ipc, store };
}

describe("CatalogStore", () => {
  it("init: summary取得・イベント購読・先頭ページ要求を正しい引数で行う", async () => {
    const { ipc, store } = setup();
    const summary = makeSummary({ generation: 7, total: 1615 });
    const page = makePage(7, 0, ["a.png", "b.png"]);
    ipc.enqueue("get_catalog_summary", summary, makeSummary({ generation: 7 }));
    ipc.enqueue("get_catalog_page", page);

    const dispose = await store.init();

    expect(ipc.argsOf("get_catalog_summary")).toEqual([undefined]);
    // ページ要求: 現在generation・offset 0・limit 200 (ページ上限の強制)
    expect(ipc.argsOf("get_catalog_page")).toEqual([
      { generation: 7, offset: 0, limit: 200 },
    ]);
    expect(store.summary).toEqual(summary);
    expect(store.pageItems.map((i) => i.displayName)).toEqual(["a.png", "b.png"]);
    expect(store.pageOffset).toBe(0);
    expect(store.pageHasMore).toBe(true);

    dispose();
    // dispose後はイベントが流れてこない
    ipc.emit("scan:progress", { scanned: 1, found: 1, errors: 0, elapsed_ms: 5, done: false });
    expect(store.progress).toBeNull();
  });

  it("loadPage: 古い要求の応答が後から届いても破棄される", async () => {
    const { ipc, store } = setup();
    ipc.enqueue("get_catalog_summary", makeSummary());
    await store.refreshSummary(); // generation 2

    const older = deferred<CatalogPage>();
    const newer = deferred<CatalogPage>();
    ipc.enqueue("get_catalog_page", () => older.promise, () => newer.promise);

    const loadOlder = store.loadPage(0);
    const loadNewer = store.loadPage(200);

    newer.resolve(makePage(2, 200, ["z.png"]));
    await loadNewer;
    expect(store.pageItems.map((i) => i.displayName)).toEqual(["z.png"]);
    expect(store.pageOffset).toBe(200);

    older.resolve(makePage(2, 0, ["a.png"]));
    await loadOlder;
    // 古い応答は上書きしない
    expect(store.pageItems.map((i) => i.displayName)).toEqual(["z.png"]);
    expect(store.pageOffset).toBe(200);
  });

  it("loadPage: 応答のgenerationが要求時と異なる場合は破棄される", async () => {
    const { ipc, store } = setup();
    ipc.enqueue("get_catalog_summary", makeSummary());
    await store.refreshSummary(); // generation 2 で要求する

    // 応答はgeneration 3 (mode switch後など) で返ってきた → 破棄
    ipc.enqueue("get_catalog_page", makePage(3, 0, ["stale.png"]));
    await store.loadPage(0);
    expect(store.pageItems).toEqual([]);
  });

  it("loadPage: staleGenerationエラーでsummary再取得→新generationで再試行する", async () => {
    const { ipc, store } = setup();
    ipc.enqueue("get_catalog_summary", makeSummary({ generation: 2 }));
    await store.refreshSummary();

    ipc.enqueue(
      "get_catalog_page",
      rejectWith({ kind: "staleGeneration", requested: 2, current: 3 }),
      makePage(3, 0, ["after-retry.png"]),
    );
    ipc.enqueue("get_catalog_summary", makeSummary({ generation: 3, mode: "virtual", total: 50000 }));

    await store.loadPage(0);

    expect(ipc.argsOf("get_catalog_summary")).toHaveLength(2);
    expect(ipc.argsOf("get_catalog_page")).toEqual([
      { generation: 2, offset: 0, limit: 200 },
      { generation: 3, offset: 0, limit: 200 },
    ]);
    expect(store.summary?.generation).toBe(3);
    expect(store.pageItems.map((i) => i.displayName)).toEqual(["after-retry.png"]);
    expect(store.lastError).toBeNull();
  });

  it("scan:progressイベントで進捗が更新され、done時にsummaryを再取得する", async () => {
    const { ipc, store } = setup();
    ipc.enqueue("get_catalog_summary", makeSummary({ generation: 2 }));
    ipc.enqueue("get_catalog_page", makePage(2, 0, ["a.png"]));
    await store.init();

    ipc.emit("scan:progress", { scanned: 64, found: 64, errors: 1, elapsed_ms: 100, done: false });
    expect(store.progress).toEqual({
      scanned: 64,
      found: 64,
      errors: 1,
      elapsed_ms: 100,
      done: false,
    });

    // summaryの再取得を予約してからdoneイベント
    ipc.enqueue("get_catalog_summary", makeSummary({ generation: 2, scanning: false, total: 1615 }));
    ipc.emit("scan:progress", { scanned: 1615, found: 1615, errors: 1, elapsed_ms: 900, done: true });
    await vi.waitFor(() => {
      expect(ipc.callsFor("get_catalog_summary").length).toBeGreaterThanOrEqual(2);
    });
  });

  it("catalog:changedイベント: generationが変わっていればsummaryを引き直してページ再取得", async () => {
    const { ipc, store } = setup();
    ipc.enqueue("get_catalog_summary", makeSummary({ generation: 2 }));
    ipc.enqueue("get_catalog_page", makePage(2, 0, ["old.png"]));
    await store.init();

    ipc.enqueue("get_catalog_summary", makeSummary({ generation: 5, mode: "virtual", total: 50000 }));
    ipc.enqueue("get_catalog_page", makePage(5, 0, ["v0.png"]));

    ipc.emit("catalog:changed", { generation: 5, reason: "mode_switch" });
    await vi.waitFor(() => {
      expect(store.summary?.generation).toBe(5);
    });
    await vi.waitFor(() => {
      expect(store.pageItems.map((i) => i.displayName)).toEqual(["v0.png"]);
    });
    // init時 (gen 2) とイベント後 (gen 5) の2回。最後が新generationで再取得されていること
    expect(ipc.argsOf("get_catalog_page")).toEqual([
      { generation: 2, offset: 0, limit: 200 },
      { generation: 5, offset: 0, limit: 200 },
    ]);
  });

  it("catalog:changedイベント: 同一generationならsummaryを取得せず現在オフセットでページ再取得", async () => {
    const { ipc, store } = setup();
    ipc.enqueue("get_catalog_summary", makeSummary({ generation: 2, scanning: true }));
    ipc.enqueue("get_catalog_page", makePage(2, 0, ["first.png"]));
    await store.init();
    // 走査中に1ページ取得しておく (offset 200)
    ipc.enqueue("get_catalog_page", makePage(2, 200, ["p.png"]));
    await store.loadPage(200);
    const summaryCalls = ipc.callsFor("get_catalog_summary").length;

    ipc.enqueue("get_catalog_page", makePage(2, 200, ["p.png", "grown.png"]));
    ipc.emit("catalog:changed", { generation: 2, reason: "scan_batch" });
    await vi.waitFor(() => {
      expect(store.pageItems.map((i) => i.displayName)).toEqual(["p.png", "grown.png"]);
    });
    // 同一generationなのでsummaryの再取得は発生しない
    expect(ipc.callsFor("get_catalog_summary").length).toBe(summaryCalls);
    expect(ipc.argsOf("get_catalog_page").at(-1)).toEqual({
      generation: 2,
      offset: 200,
      limit: 200,
    });
  });

  it("setMode: 仮想カタログへ切り替え、新summary適用と先頭ページ再取得を行う", async () => {
    const { ipc, store } = setup();
    ipc.enqueue("get_catalog_summary", makeSummary({ generation: 2 }));
    await store.refreshSummary();

    const virtualSummary = makeSummary({ generation: 3, mode: "virtual", total: 50000 });
    ipc.enqueue("set_catalog_mode", virtualSummary);
    ipc.enqueue("get_catalog_page", makePage(3, 0, ["virtual-00000-a.png"]));

    const ok = await store.setMode(true);
    expect(ok).toBe(true);
    expect(ipc.argsOf("set_catalog_mode")).toEqual([{ virtual: true }]);
    expect(store.summary).toEqual(virtualSummary);
    expect(store.pageItems.map((i) => i.displayName)).toEqual(["virtual-00000-a.png"]);
    expect(store.lastError).toBeNull();
  });

  it("setMode: 実走査前に仮想へ切り替えた場合はエラーをlastErrorへ置く", async () => {
    const { ipc, store } = setup();
    ipc.enqueue("get_catalog_summary", makeSummary({ generation: 1, total: 0 }));
    await store.refreshSummary();

    ipc.enqueue("set_catalog_mode", new Error("real catalog is empty; run start_scan first"));
    const ok = await store.setMode(true);
    expect(ok).toBe(false);
    expect(store.lastError).toContain("real catalog is empty");
  });

  it("startScan: 二重走査エラーをlastErrorへ置き、falseを返す", async () => {
    const { ipc, store } = setup();
    ipc.enqueue("start_scan", new Error("scan already in progress"));
    const ok = await store.startScan();
    expect(ok).toBe(false);
    expect(store.lastError).toBe("scan already in progress");
    expect(store.progress).toBeNull();
  });

  it("startScan: 成功時はsummaryを再取得する", async () => {
    const { ipc, store } = setup();
    ipc.enqueue("get_catalog_summary", makeSummary({ generation: 2, scanning: true }));
    await store.refreshSummary();
    ipc.enqueue("start_scan", null);
    ipc.enqueue("get_catalog_summary", makeSummary({ generation: 3, scanning: true, total: 0 }));

    const ok = await store.startScan();
    expect(ok).toBe(true);
    expect(store.summary?.generation).toBe(3);
    expect(store.summary?.scanning).toBe(true);
  });

  it("onChange: 状態変化ごとに通知され、解除すると来なくなる", async () => {
    const { ipc, store } = setup();
    const snapshots: number[] = [];
    const stop = store.onChange(() => snapshots.push((store.summary?.generation ?? 0)));

    ipc.enqueue("get_catalog_summary", makeSummary({ generation: 2 }));
    await store.refreshSummary();
    ipc.enqueue("get_catalog_page", makePage(2, 0, ["a.png"]));
    await store.loadPage(0);
    expect(snapshots.length).toBeGreaterThanOrEqual(2);

    stop();
    const count = snapshots.length;
    ipc.enqueue("get_catalog_summary", makeSummary({ generation: 3 }));
    await store.refreshSummary();
    expect(snapshots.length).toBe(count);
  });
});
