import { describe, expect, it, vi } from "vitest";
import {
  EMPTY_FILTERS,
  SearchStore,
  filterTerms,
  filtersActive,
  type GuiFilters,
} from "./search";
import { semanticPercent, weightLabel } from "./search";
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
              kind: "query",
              semanticScores: { 7: 0.9 },
            } as SearchRunResult as T;
          }
          if (command === "similar_search_run") {
            return {
              summary: okSummary(),
              total: 3,
              shown: 3,
              elapsedMs: 8,
              warnings: [],
              queryJson: "",
              kind: "similar",
              semanticScores: { 7: 0.95, 8: 0.8 },
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

  it("run: semanticWeightを0..1に拘束して渡す (WP-12)", async () => {
    const { ipc, calls } = makeIpc();
    const store = new SearchStore(ipc);
    await store.run("猫", EMPTY_FILTERS, {}, 0.65);
    const run = calls.find((c) => c.command === "search_run");
    expect(run?.args?.semanticWeight).toBeCloseTo(0.65);
    expect(store.lastSemanticWeight).toBe(0.65);
    // 範囲外はclamp
    await store.run("猫", EMPTY_FILTERS, {}, 7);
    const calls2 = calls.filter((c) => c.command === "search_run");
    expect(calls2[calls2.length - 1]?.args?.semanticWeight).toBe(1);
    await store.run("猫", EMPTY_FILTERS, {}, -2);
    const calls3 = calls.filter((c) => c.command === "search_run");
    expect(calls3[calls3.length - 1]?.args?.semanticWeight).toBe(0);
  });

  it("run: 意味スコア写像を保持する (WP-12)", async () => {
    const { ipc } = makeIpc();
    const store = new SearchStore(ipc);
    await store.run("猫", EMPTY_FILTERS, {}, 0.5);
    expect(store.semantic["7"]).toBeCloseTo(0.9);
    expect(store.lastKind).toBe("query");
  });

  it("runSimilar: 類似画像検索を実行しkind/スコアを保持 (WP-12)", async () => {
    const { ipc, calls } = makeIpc();
    const store = new SearchStore(ipc);
    const ok = await store.runSimilar("42");
    expect(ok).toBe(true);
    const call = calls.find((c) => c.command === "similar_search_run");
    expect(call?.args?.assetId).toBe("42");
    expect(store.lastKind).toBe("similar");
    expect(store.semantic["7"]).toBeCloseTo(0.95);
    // 失敗はlastErrorへ
    const failIpc = {
      invoke: async () => {
        throw new Error("この画像には埋め込みがありません");
      },
    };
    const failStore = new SearchStore(failIpc);
    const failOk = await failStore.runSimilar("42");
    expect(failOk).toBe(false);
    expect(failStore.lastError).toContain("埋め込みがありません");
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

describe("semanticPercent / weightLabel — 表示ヘルパー (WP-12)", () => {
  it("semanticPercent: 0..1を%表示へ (負・過大は拘束)", () => {
    expect(semanticPercent(0.87)).toBe("87%");
    expect(semanticPercent(0)).toBe("0%");
    expect(semanticPercent(1)).toBe("100%");
    expect(semanticPercent(-0.2)).toBe("0%");
    expect(semanticPercent(1.7)).toBe("100%");
    expect(semanticPercent(Number.NaN)).toBe("0%");
    expect(semanticPercent(0.871)).toBe("87%");
  });

  it("weightLabel: 重みの段階ラベル", () => {
    expect(weightLabel(0)).toBe("キーワード");
    expect(weightLabel(0.2)).toBe("キーワード寄り");
    expect(weightLabel(0.5)).toBe("バランス");
    expect(weightLabel(0.8)).toBe("意味寄り");
    expect(weightLabel(1)).toBe("意味");
  });
});
