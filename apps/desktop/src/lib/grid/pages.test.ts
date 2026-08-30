import { describe, expect, it, vi } from "vitest";
import { CatalogPager, pagesForIndexRange } from "./pages";
import type { CatalogPage, CatalogSummary } from "../catalog-types";

describe("pagesForIndexRange", () => {
  it("範囲を含むページと前後1ページを返す", () => {
    expect(pagesForIndexRange(0, 10, 50_000, 200)).toEqual([0, 1]);
    expect(pagesForIndexRange(199, 200, 50_000, 200)).toEqual([0, 1, 2]);
  });

  it("末尾では最終ページ以降を返さない", () => {
    expect(pagesForIndexRange(49_999, 49_999, 50_000, 200)).toEqual([248, 249]);
  });

  it("先頭では負のページを返さない", () => {
    // total=100 < pageSize=200 なので存在するのはpage 0のみ
    expect(pagesForIndexRange(0, 0, 100, 200)).toEqual([0]);
    expect(pagesForIndexRange(0, 0, 1, 200)).toEqual([0]);
  });

  it("空の範囲・空カタログは空配列", () => {
    expect(pagesForIndexRange(10, 5, 100, 200)).toEqual([]);
    expect(pagesForIndexRange(0, 10, 0, 200)).toEqual([]);
  });
});

type InvokeCall = { command: string; args?: Record<string, unknown> };

function makePage(offset: number, count: number, total: number, generation: number): CatalogPage {
  const items = Array.from({ length: count }, (_, i) => ({
    id: String(offset + i),
    displayName: `item-${offset + i}`,
    width: 100,
    height: 100,
    byteSize: 1000,
    state: "ready" as const,
    thumbnailState: "missing" as const,
    isAnimated: false,
    version: 800_000 + offset + i,
  }));
  return { generation, total, offset, items, has_more: offset + count < total };
}

function makeIpc(
  pages: Map<number, CatalogPage>,
  calls: InvokeCall[],
  opts?: { failOnce?: () => boolean; summaryGeneration?: number },
) {
  return {
    invoke: async <T>(command: string, args?: Record<string, unknown>): Promise<T> => {
      calls.push({ command, args });
      if (command === "get_catalog_summary") {
        return {
          generation: opts?.summaryGeneration ?? 1,
          mode: "virtual",
          total: 50_000,
          scanning: false,
          scanned_count: 1615,
          error_count: 0,
          last_scan_elapsed_ms: 1,
        } as T;
      }
      if (command === "get_catalog_page") {
        if (opts?.failOnce?.()) {
          throw "network down";
        }
        const page = pages.get(args?.offset as number);
        if (!page) {
          throw { kind: "invalidRange", message: "no fixture" };
        }
        return page as T;
      }
      throw `unexpected command ${command}`;
    },
  };
}

describe("CatalogPager", () => {
  it("ensureRangeは範囲に必要なページを前後1ページ余裕で取得する", async () => {
    const calls: InvokeCall[] = [];
    const pages = new Map<number, CatalogPage>([
      [0, makePage(0, 200, 50_000, 1)],
      [200, makePage(200, 200, 50_000, 1)],
      [400, makePage(400, 200, 50_000, 1)],
    ]);
    const pager = new CatalogPager(makeIpc(pages, calls));
    pager.reset(1, 50_000);
    await pager.ensureRange(0, 10);
    expect(calls.filter((c) => c.command === "get_catalog_page")).toHaveLength(2);
    expect(pager.getItem(0)?.id).toBe("0");
    expect(pager.stats.fetched).toBe(2);
  });

  it("ページ境界をまたいで項目を解決する", async () => {
    const calls: InvokeCall[] = [];
    const pages = new Map<number, CatalogPage>([
      [0, makePage(0, 200, 50_000, 1)],
      [200, makePage(200, 200, 50_000, 1)],
      [400, makePage(400, 200, 50_000, 1)],
      [600, makePage(600, 200, 50_000, 1)],
    ]);
    const pager = new CatalogPager(makeIpc(pages, calls));
    pager.reset(1, 50_000);
    await pager.ensureRange(199, 201);
    expect(pager.getItem(199)?.id).toBe("199");
    expect(pager.getItem(200)?.id).toBe("200");
    expect(pager.getItem(201)?.id).toBe("201");
    // marginで取得したページ (offset 400) は解決できる
    expect(pager.getItem(450)?.id).toBe("450");
    // 未取得ページはnull
    expect(pager.getItem(650)).toBeNull();
  });

  it("同一ページへの並行ensureRangeは1回だけfetchする", async () => {
    const calls: InvokeCall[] = [];
    const pages = new Map<number, CatalogPage>([
      [0, makePage(0, 200, 50_000, 1)],
      [200, makePage(200, 200, 50_000, 1)],
    ]);
    const pager = new CatalogPager(makeIpc(pages, calls));
    pager.reset(1, 50_000);
    await Promise.all([pager.ensureRange(0, 5), pager.ensureRange(2, 8)]);
    expect(calls.filter((c) => c.command === "get_catalog_page")).toHaveLength(2);
  });

  it("取得済みページの再取得はしない", async () => {
    const calls: InvokeCall[] = [];
    const pages = new Map<number, CatalogPage>([
      [0, makePage(0, 200, 50_000, 1)],
      [200, makePage(200, 200, 50_000, 1)],
    ]);
    const pager = new CatalogPager(makeIpc(pages, calls));
    pager.reset(1, 50_000);
    await pager.ensureRange(0, 5);
    await pager.ensureRange(0, 50);
    expect(calls.filter((c) => c.command === "get_catalog_page")).toHaveLength(2);
  });

  it("generation変化のresetはキャッシュをクリアする", async () => {
    const calls: InvokeCall[] = [];
    const pages = new Map<number, CatalogPage>([
      [0, makePage(0, 200, 50_000, 2)],
      [200, makePage(200, 200, 50_000, 2)],
    ]);
    const pager = new CatalogPager(makeIpc(pages, calls));
    pager.reset(1, 50_000);
    await pager.ensureRange(0, 5);
    pager.reset(2, 50_000);
    expect(pager.generation).toBe(2);
    expect(pager.getItem(0)).toBeNull();
  });

  it("onResetはgenerationが変わった時だけ呼ばれる", () => {
    const pager = new CatalogPager(makeIpc(new Map(), []));
    const resets: number[] = [];
    pager.onReset = (gen) => resets.push(gen);
    pager.reset(1, 100);
    pager.reset(1, 200); // totalのみ変化 (走査成長)
    pager.reset(2, 200);
    expect(resets).toEqual([1, 2]);
    expect(pager.total).toBe(200);
  });

  it("findIndicesByIdsはキャッシュ済みページからid→indexを解決する", async () => {
    const calls: InvokeCall[] = [];
    const pages = new Map<number, CatalogPage>([
      [0, makePage(0, 200, 50_000, 1)],
      [200, makePage(200, 200, 50_000, 1)],
    ]);
    const pager = new CatalogPager(makeIpc(pages, calls));
    pager.reset(1, 50_000);
    await pager.ensureRange(0, 5);
    const found = pager.findIndicesByIds(new Set(["42", "250", "999"]));
    expect(found.get("42")).toBe(42);
    expect(found.get("250")).toBe(250);
    // 未取得ページのidは結果に含まれない
    expect(found.has("999")).toBe(false);
    expect(pager.findIndicesByIds(new Set())).toEqual(new Map());
  });

  it("staleGenerationはsummary再取得のうえ1回再試行する", async () => {
    const calls: InvokeCall[] = [];
    const pages = new Map<number, CatalogPage>([
      [0, makePage(0, 200, 50_000, 2)],
      [200, makePage(200, 200, 50_000, 2)],
    ]);
    let staleOnce = true;
    const ipc = {
      invoke: async <T>(command: string, args?: Record<string, unknown>): Promise<T> => {
        calls.push({ command, args });
        if (command === "get_catalog_page" && staleOnce) {
          staleOnce = false;
          throw { kind: "staleGeneration", requested: 1, current: 2 };
        }
        if (command === "get_catalog_summary") {
          return { generation: 2, mode: "virtual", total: 50_000 } as CatalogSummary as T;
        }
        return pages.get(args?.offset as number) as T;
      },
    };
    const pager = new CatalogPager(ipc);
    const resets: number[] = [];
    pager.onReset = (gen) => resets.push(gen);
    pager.reset(1, 50_000);
    await pager.ensureRange(0, 5);
    expect(pager.generation).toBe(2);
    expect(resets).toEqual([1, 2]);
    expect(pager.getItem(0)?.id).toBe("0");
    // 1回目 (stale) + 並行page1 + 再試行。再試行は新generationで行う
    const pageCalls = calls.filter((c) => c.command === "get_catalog_page");
    expect(pageCalls).toHaveLength(3);
    expect(pageCalls[2]?.args?.generation).toBe(2);
    expect(pageCalls[2]?.args?.offset).toBe(0);
  });

  it("エラー時はlastErrorを設定し次回再試行する", async () => {
    const calls: InvokeCall[] = [];
    const pages = new Map<number, CatalogPage>([
      [0, makePage(0, 200, 50_000, 1)],
      [200, makePage(200, 200, 50_000, 1)],
    ]);
    let failing = true;
    const ipc = makeIpc(pages, calls, { failOnce: () => failing });
    const pager = new CatalogPager(ipc);
    pager.reset(1, 50_000);
    await pager.ensureRange(0, 5);
    expect(pager.lastError).toBe("network down");
    expect(pager.getItem(0)).toBeNull();
    failing = false;
    await pager.ensureRange(0, 5);
    expect(pager.getItem(0)?.id).toBe("0");
  });

  it("応答喪失 (永久に未解決のinvoke) はタイムアウトで復旧し再試行できる", async () => {
    vi.useFakeTimers();
    try {
      const pages = new Map<number, CatalogPage>([
        [0, makePage(0, 200, 50_000, 1)],
      ]);
      let lost = true;
      const calls: Array<{ command: string; args?: Record<string, unknown> }> = [];
      const ipc = {
        invoke: <T>(command: string, args?: Record<string, unknown>): Promise<T> => {
          calls.push({ command, args });
          if (command === "get_catalog_page" && (args?.offset as number) === 200 && lost) {
            // 応答が返らない (失われたIPC応答を模倣)
            return new Promise<T>(() => {});
          }
          const page = pages.get(args?.offset as number);
          return Promise.resolve((page ?? makePage(args?.offset as number, 200, 50_000, 1)) as T);
        },
      };
      const pager = new CatalogPager(ipc, { timeoutMs: 100 });
      pager.reset(1, 50_000);
      // page 0,1 が必要 (offset 0 と 200)。page 1 は応答喪失。
      const pending = pager.ensureRange(0, 5);
      const settled = await vi.advanceTimersByTimeAsync(100).then(() => pending);
      expect(settled).toBeUndefined(); // タイムアウトでもensureRange自体は完了する
      expect(pager.lastError).toContain("timed out");
      expect(pager.getItem(0)?.id).toBe("0"); // page 0は利用可
      expect(pager.getItem(200)).toBeNull(); // page 1は未キャッシュ
      // 再試行は新しいinvokeを発行する
      const before = calls.filter((c) => c.command === "get_catalog_page").length;
      lost = false;
      await pager.ensureRange(200, 210);
      expect(
        calls.filter((c) => c.command === "get_catalog_page").length,
      ).toBeGreaterThan(before);
      expect(pager.getItem(200)?.id).toBe("200");
    } finally {
      vi.useRealTimers();
    }
  });
});
