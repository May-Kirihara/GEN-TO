// @vitest-environment happy-dom
/**
 * SearchBar.svelte コンポーネントテスト (UIUX-7/WP1)。
 *
 * - 3モードセグメント (keyword/hybrid/semantic): 描画・aria-pressed選択状態・
 *   クリックでonSemanticModeChange発火
 * - 不可用时: hybrid/semanticはdisabled + 自身のtitleに機能説明と
 *   semanticReasonを改行連結 (WP5/QA Major — spanのtitleは子button優先で
 *   ホバー到達しないため理由をbutton側で出す)。keywordは意味検索に
 *   依存しないため常時enabled・理由の連結も受けない
 * - サジェストチップ (semantic-suggest-chip): 表示条件はApp側で計算済みの
 *   showSemanticSuggestをpropsで受ける — ここは表示・クリック発火のみ検証。
 *   aria-label=意味検索を試す (emoji読み上げ回避 — WP5)
 * - 検索inputのplaceholder文言 (自然言語例文+構文例)
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import SearchBar from "./SearchBar.svelte";
import { EMPTY_FILTERS } from "../search";
import type { CollapsedSearchSummary } from "../ui/search-summary";
import type { SemanticMode } from "../ui/search-mode";

let target: HTMLElement | null = null;
let component: Record<string, unknown> | null = null;
let modeChanges: SemanticMode[] = [];
let suggestClicks = 0;

const SUMMARY: CollapsedSearchSummary = {
  queryText: "",
  scopeLabel: null,
  badges: [],
  showClear: false,
};

function mountBar(
  overrides: {
    semanticMode?: SemanticMode;
    semanticAvailable?: boolean;
    semanticReason?: string;
    showSemanticSuggest?: boolean;
  } = {},
): void {
  // 同一ファイル内で複数回mountするため、旧targetを先に除去する
  target?.remove();
  target = document.createElement("div");
  document.body.appendChild(target);
  component = mount(SearchBar, {
    target,
    props: {
      searchExpanded: true,
      onToggleSearchExpanded: () => {},
      collapsedSearchLine: SUMMARY,
      onClearSearchFromCollapsed: () => {},
      searchRunning: false,
      searchInput: "",
      onSearchInput: () => {},
      onRunSearch: () => {},
      searchScopeMode: "library",
      onSearchScopeChange: () => {},
      semanticAvailable: overrides.semanticAvailable ?? true,
      semanticReason: overrides.semanticReason ?? "",
      semanticMode: overrides.semanticMode ?? "keyword",
      onSemanticModeChange: (mode: SemanticMode) => modeChanges.push(mode),
      showSemanticSuggest: overrides.showSemanticSuggest ?? false,
      onSemanticSuggestClick: () => (suggestClicks += 1),
      searchFiltersOpen: false,
      onToggleSearchFilters: () => {},
      onCloseSearchFilters: () => {},
      searchFiltersActive: false,
      searchWarnings: [],
      canSaveSearch: false,
      onSaveCurrentSearch: () => {},
      searchFilters: { ...EMPTY_FILTERS },
      onSearchFiltersChange: () => {},
      searchFilterChipList: [],
      onRemoveSearchFilterChip: () => {},
    },
  });
  flushSync();
}

function segment(): HTMLElement {
  const el = target!.querySelector('[data-testid="search-mode-segment"]');
  if (!(el instanceof HTMLElement)) {
    throw new Error("search-mode-segment が描画されていません");
  }
  return el;
}

function modeButton(mode: SemanticMode): HTMLButtonElement {
  const el = target!.querySelector(`[data-testid="search-mode-${mode}"]`);
  if (!(el instanceof HTMLButtonElement)) {
    throw new Error(`search-mode-${mode} ボタンが描画されていません`);
  }
  return el;
}

beforeEach(() => {
  modeChanges = [];
  suggestClicks = 0;
});

afterEach(() => {
  if (component) {
    unmount(component);
    component = null;
  }
  target?.remove();
  target = null;
});

describe("SearchBar — 3モードセグメント (UIUX-7/A)", () => {
  it("キーワード/ハイブリッド/意味の3ボタンがsearch-mode-segment内に描画される", () => {
    mountBar();
    const buttons = segment().querySelectorAll("button");
    expect(buttons).toHaveLength(3);
    expect(modeButton("keyword").textContent?.trim()).toBe("キーワード");
    expect(modeButton("hybrid").textContent?.trim()).toBe("ハイブリッド");
    expect(modeButton("semantic").textContent?.trim()).toBe("意味");
  });

  it("semanticModeに対応するボタンがaria-pressed=true (他はfalse)", () => {
    mountBar({ semanticMode: "hybrid" });
    expect(modeButton("keyword").getAttribute("aria-pressed")).toBe("false");
    expect(modeButton("hybrid").getAttribute("aria-pressed")).toBe("true");
    expect(modeButton("semantic").getAttribute("aria-pressed")).toBe("false");
    mountBar({ semanticMode: "semantic" });
    expect(modeButton("semantic").getAttribute("aria-pressed")).toBe("true");
    expect(modeButton("hybrid").getAttribute("aria-pressed")).toBe("false");
  });

  it("不可用时はkeywordのみenabled・hybrid/semanticはdisabled + title=semanticReason (可用時は全enabled)", () => {
    const reason = "自然言語検索は準備中です (索引構築中)";
    mountBar({ semanticAvailable: false, semanticReason: reason });
    // keywordは意味検索不可でも常に使える (UIUX-7/WP2)
    expect(modeButton("keyword").disabled).toBe(false);
    expect(modeButton("hybrid").disabled).toBe(true);
    expect(modeButton("semantic").disabled).toBe(true);
    expect(segment().getAttribute("title")).toBe(reason);
    mountBar({ semanticAvailable: true });
    expect(modeButton("keyword").disabled).toBe(false);
    expect(modeButton("hybrid").disabled).toBe(false);
    expect(modeButton("semantic").disabled).toBe(false);
    expect(segment().getAttribute("title")).not.toBe(reason);
  });

  it("不可用时のhybrid/semanticボタンtitleは機能説明+semanticReasonの改行連結 (可用時は理由を含まない)", () => {
    const reason = "自然言語検索は準備中です (索引構築中)";
    mountBar({ semanticAvailable: false, semanticReason: reason });
    // WP5/QA Major: 不可の理由はdisabledボタン自身のtitleでホバー到達可能であること
    expect(modeButton("hybrid").title).toBe(
      "キーワード検索と意味検索 (自然言語) を半々で融合\n" + reason,
    );
    expect(modeButton("semantic").title).toBe(
      "自然言語の意味の近さだけで検索 (キーワード一致は使わない)\n" + reason,
    );
    // keywordは常時有効のため理由の連結を受けない
    expect(modeButton("keyword").title).not.toContain(reason);
    mountBar({ semanticAvailable: true, semanticReason: reason });
    expect(modeButton("hybrid").title).not.toContain(reason);
    expect(modeButton("semantic").title).not.toContain(reason);
  });

  it("不可用时でもkeywordclickでonSemanticModeChange(keyword)が発火する", () => {
    mountBar({ semanticAvailable: false, semanticMode: "semantic" });
    modeButton("keyword").click();
    flushSync();
    expect(modeChanges).toEqual(["keyword"]);
  });

  it("ボタンclickでonSemanticModeChangeが対応するmodeで発火する", () => {
    mountBar({ semanticMode: "keyword" });
    modeButton("hybrid").click();
    modeButton("semantic").click();
    modeButton("keyword").click();
    flushSync();
    expect(modeChanges).toEqual(["hybrid", "semantic", "keyword"]);
  });
});

describe("SearchBar — 意味検索サジェストチップ (UIUX-7/C)", () => {
  it("showSemanticSuggest=trueのとき「🧠 意味検索を試す」チップを表示・clickで発火", () => {
    mountBar({ showSemanticSuggest: true });
    const chip = target!.querySelector<HTMLButtonElement>(
      '[data-testid="semantic-suggest-chip"]',
    );
    expect(chip).not.toBeNull();
    expect(chip!.textContent?.trim()).toBe("🧠 意味検索を試す");
    // WP5: emoji (🧠) を読み上げさせない明示的アクセシブル名
    expect(chip!.getAttribute("aria-label")).toBe("意味検索を試す");
    chip!.click();
    flushSync();
    expect(suggestClicks).toBe(1);
  });

  it("showSemanticSuggest=falseのときチップは非表示", () => {
    mountBar({ showSemanticSuggest: false });
    expect(target!.querySelector('[data-testid="semantic-suggest-chip"]')).toBeNull();
  });
});

describe("SearchBar — placeholder (UIUX-7/D)", () => {
  it("自然言語例文+構文例のplaceholder文言", () => {
    mountBar();
    const input = target!.querySelector<HTMLInputElement>(
      '[data-testid="search-input"]',
    );
    expect(input?.placeholder).toBe(
      "検索 (自然言語OK: 夕暮れの海岸 / 構文: tag:night rating:>=3)",
    );
  });
});
