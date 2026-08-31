import { describe, expect, it, vi } from "vitest";
import {
  EMPTY_FILTERS,
  SearchStore,
  filterTerms,
  filtersActive,
  type GuiFilters,
} from "./search";
import type { SearchEvidenceItem, SearchRunResult } from "./search";

function okSummary() {
  return {
    generation: 3,
    mode: "library" as const,
    total: 12,
    scanning: false,
    scanned_count: 0,
    error_count: 0,
    last_scan_elapsed_ms: null,
    sort: "name" as const,
  };
}

describe("filterTerms — GUIフィルタのAST合成", () => {
  it("未設定フィルタは空リスト", () => {
    expect(filterTerms(EMPTY_FILTERS)).toEqual([]);
    expect(filtersActive(EMPTY_FILTERS)).toBe(false);
  });

  it("各条件のAST形状 (gento-search AST直列化と同一)", () => {
    const filters: GuiFilters = {
      ratingMin: 3,
      cull: "picked",
      favorite: true,
      type: "png",
      tag: "Night",
      addedFrom: "2026-01-01",
      addedTo: "2026-06-30",
    };
    expect(filtersActive(filters)).toBe(true);
    expect(filterTerms(filters)).toEqual([
      { type: "field", key: "rating", op: "ge", value: { number: 3 } },
      { type: "field", key: "cull", op: "eq", value: { text: "picked" } },
      { type: "field", key: "fav", op: "eq", value: { bool: true } },
      { type: "field", key: "type", op: "eq", value: { text: "png" } },
      { type: "field", key: "tag", op: "eq", value: { text: "Night" } },
      {
        type: "field",
        key: "added",
        op: "eq",
        value: {
          date_range: {
            from: { year: 2026, month: 1, day: 1 },
            to: { year: 2026, month: 6, day: 30 },
          },
        },
      },
    ]);
  });

  it("日付範囲は片端のみでも構成できる", () => {
    const terms = filterTerms({ ...EMPTY_FILTERS, addedTo: "2026-06-30" });
    expect(terms).toEqual([
      {
        type: "field",
        key: "added",
        op: "eq",
        value: { date_range: { from: null, to: { year: 2026, month: 6, day: 30 } } },
      },
    ]);
  });

  it("不正な日付形式はエラー (backendへ曖昧な値を送らない)", () => {
    expect(() => filterTerms({ ...EMPTY_FILTERS, addedFrom: "2026/01/01" })).toThrow(
      "YYYY-MM-DD",
    );
    expect(() => filterTerms({ ...EMPTY_FILTERS, addedTo: "令和8年" })).toThrow("YYYY-MM-DD");
  });
});

describe("SearchStore — 実行・根拠・保存", () => {
  function makeIpc() {
    const calls: Array<{ command: string; args?: Record<string, unknown> }> = [];
    return {
      calls,
      ipc: {
        invoke: async <T>(command: string, args?: Record<string, unknown>): Promise<T> => {
          calls.push({ command, args });
          if (command === "search_run") {
            return {
              summary: okSummary(),
              total: 12,
              shown: 12,
              elapsedMs: 5,
              warnings: [],
              queryJson: '{"version":2,"query":{"terms":[]}}',
            } as SearchRunResult as T;
          }
          if (command === "search_evidence") {
            const ids = (args?.assetIds as string[]) ?? [];
            return ids.map((assetId, i) => ({
              assetId,
              matches: [{ field: "tags", snippet: `…[hit${i}]…` }],
            })) as SearchEvidenceItem[] as T;
          }
          return undefined as T;
        },
      },
    };
  }

  it("run: クエリ+フィルタ+スコープを渡し結果を保持する", async () => {
    const { ipc, calls } = makeIpc();
    const store = new SearchStore(ipc);
    const ok = await store.run(
      "夜景 rating:>=3",
      { ...EMPTY_FILTERS, favorite: true },
      { rootId: "7" },
    );
    expect(ok).toBe(true);
    expect(store.lastError).toBeNull();
    expect(store.lastResult?.total).toBe(12);
    expect(store.lastQuery).toBe("夜景 rating:>=3");
    const run = calls.find((c) => c.command === "search_run");
    expect(run?.args?.query).toBe("夜景 rating:>=3");
    expect(run?.args?.scopeRootId).toBe("7");
    expect(run?.args?.filters).toEqual([
      { type: "field", key: "fav", op: "eq", value: { bool: true } },
    ]);
  });

  it("run: 構文エラーはlastErrorへ (throwしない)", async () => {
    const ipc = {
      invoke: async () => {
        throw new Error("不明な検索キーです: foo (位置: 0文字目)");
      },
    };
    const store = new SearchStore(ipc);
    const ok = await store.run("foo:bar", EMPTY_FILTERS);
    expect(ok).toBe(false);
    expect(store.lastError).toContain("不明な検索キー");
  });

  it("evidenceFor: 未取得のIDのみ要求し写像へ保持", async () => {
    const { ipc, calls } = makeIpc();
    const store = new SearchStore(ipc);
    await store.run("夜景", EMPTY_FILTERS);
    await store.evidenceFor(["1", "2"]);
    expect(store.evidence["1"]).toEqual([{ field: "tags", snippet: "…[hit0]…" }]);
    // 2回目は取得済みIDを要求しない
    const before = calls.filter((c) => c.command === "search_evidence").length;
    await store.evidenceFor(["1"]);
    const after = calls.filter((c) => c.command === "search_evidence").length;
    expect(after).toBe(before);
    // evidenceの引数はcamelCase (assetIds)
    const evidenceCall = calls.find((c) => c.command === "search_evidence");
    expect(evidenceCall?.args?.assetIds).toEqual(["1", "2"]);
  });

  it("save: 保存済み検索はsmart_collection_createのsearch_jsonへ", async () => {
    const { ipc, calls } = makeIpc();
    const store = new SearchStore(ipc);
    const ok = await store.save("夜の写真", '{"version":2,"query":{"terms":[]}}');
    expect(ok).toBe(true);
    const call = calls.find((c) => c.command === "smart_collection_create");
    expect(call?.args?.name).toBe("夜の写真");
    expect(call?.args?.searchJson).toBe('{"version":2,"query":{"terms":[]}}');
  });

  it("onChange: 実行状態の変化を通知する", async () => {
    const { ipc } = makeIpc();
    const store = new SearchStore(ipc);
    const listener = vi.fn();
    store.onChange(listener);
    await store.run("sunset", EMPTY_FILTERS);
    expect(listener.mock.calls.length).toBeGreaterThanOrEqual(2);
  });
});
