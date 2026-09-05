import { describe, expect, it } from "vitest";
import { collapsedSearchSummary, type CommittedSearchState } from "./search-summary";

function state(overrides: Partial<CommittedSearchState> = {}): CommittedSearchState {
  return {
    kind: "none",
    query: "",
    scopeLabel: null,
    semanticWeightPercent: 0,
    filtersActive: false,
    running: false,
    active: false,
    ...overrides,
  };
}

describe("collapsedSearchSummary (UIUX-3 V-3: 折りたたみ要約)", () => {
  it("未実行 (kind=none) は検索語なし・badgeなし・クリア操作なし", () => {
    const summary = collapsedSearchSummary(state());
    expect(summary.queryText).toBe("");
    expect(summary.scopeLabel).toBeNull();
    expect(summary.badges).toEqual([]);
    expect(summary.showClear).toBe(false);
  });

  it("クエリ検索は確定済み検索語とscopeラベルを表示", () => {
    const summary = collapsedSearchSummary(
      state({
        kind: "query",
        query: "青い髪 tag:night",
        scopeLabel: "ライブラリ全体",
      }),
    );
    expect(summary.queryText).toBe("青い髪 tag:night");
    expect(summary.scopeLabel).toBe("ライブラリ全体");
    expect(summary.badges).toEqual([]);
  });

  it("類似画像検索は検索語の代わりに「類似画像」と表示", () => {
    const summary = collapsedSearchSummary(
      state({ kind: "similar", query: "asset:12345 (類似)", scopeLabel: "ライブラリ全体" }),
    );
    expect(summary.queryText).toBe("類似画像");
  });

  it("scopeラベルは実態名をそのまま使う (登録ルート/コレクション)", () => {
    const root = collapsedSearchSummary(
      state({ kind: "query", query: "x", scopeLabel: "登録ルート: /photos/2026" }),
    );
    expect(root.scopeLabel).toBe("登録ルート: /photos/2026");
    const coll = collapsedSearchSummary(
      state({ kind: "query", query: "x", scopeLabel: "コレクション: 選別中" }),
    );
    expect(coll.scopeLabel).toBe("コレクション: 選別中");
  });

  it("フィルター有効と意味重み>0はbadge表示 (重み0は非表示)", () => {
    expect(
      collapsedSearchSummary(
        state({ kind: "query", query: "x", scopeLabel: "ライブラリ全体", filtersActive: true }),
      ).badges,
    ).toEqual(["フィルター"]);
    expect(
      collapsedSearchSummary(
        state({ kind: "query", query: "x", scopeLabel: "ライブラリ全体", semanticWeightPercent: 40 }),
      ).badges,
    ).toEqual(["意味 40%"]);
    expect(
      collapsedSearchSummary(
        state({
          kind: "query",
          query: "x",
          scopeLabel: "ライブラリ全体",
          filtersActive: true,
          semanticWeightPercent: 100,
        }),
      ).badges,
    ).toEqual(["フィルター", "意味 100%"]);
    expect(
      collapsedSearchSummary(state({ kind: "query", query: "x", scopeLabel: "ライブラリ全体" }))
        .badges,
    ).toEqual([]);
  });

  it("検索実行中または検索結果表示中はクリア操作を表示", () => {
    expect(collapsedSearchSummary(state({ running: true })).showClear).toBe(true);
    expect(collapsedSearchSummary(state({ active: true })).showClear).toBe(true);
    expect(collapsedSearchSummary(state({ running: false, active: false })).showClear).toBe(false);
  });
});
