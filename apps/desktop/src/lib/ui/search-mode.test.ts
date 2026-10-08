/**
 * search-mode.ts 純関数テスト (UIUX-7/WP1 — test-first)。
 *
 * - モード↔重み変換 (keyword=0 / hybrid=50 / semantic=100)
 * - ラベル変換 (キーワード/ハイブリッド/意味 + 折りたたみbadge用「類似」)
 * - 永続化値バリデーション (不正値→keyword)
 * - SemanticModeStoreの保存・復元 (KeyValueStorage注入)
 */
import { describe, expect, it } from "vitest";
import {
  MODE_WEIGHT_PERCENT,
  SEMANTIC_MODE_STORAGE_KEY,
  SemanticModeStore,
  coerceSemanticMode,
  searchBadgeLabel,
  semanticModeLabel,
  semanticModeOfWeightPercent,
} from "./search-mode";
import type { KeyValueStorage } from "./layout-prefs";

/** localStorage互換のインメモリStorageモック。 */
function memoryStorage(initial: Record<string, string> = {}): KeyValueStorage {
  const map = new Map(Object.entries(initial));
  return {
    getItem: (key) => map.get(key) ?? null,
    setItem: (key, value) => void map.set(key, value),
  };
}

describe("MODE_WEIGHT_PERCENT — モード→実行重み (0/50/100)", () => {
  it("3モードの重みは 0 / 50 / 100", () => {
    expect(MODE_WEIGHT_PERCENT.keyword).toBe(0);
    expect(MODE_WEIGHT_PERCENT.hybrid).toBe(50);
    expect(MODE_WEIGHT_PERCENT.semantic).toBe(100);
  });
});

describe("coerceSemanticMode — 永続化値の検証", () => {
  it("正当なモード文字列はそのまま通す", () => {
    expect(coerceSemanticMode("keyword")).toBe("keyword");
    expect(coerceSemanticMode("hybrid")).toBe("hybrid");
    expect(coerceSemanticMode("semantic")).toBe("semantic");
  });

  it("未知・破損値はkeywordへフォールバック", () => {
    expect(coerceSemanticMode("meaning")).toBe("keyword");
    expect(coerceSemanticMode("")).toBe("keyword");
    expect(coerceSemanticMode("KEYWORD")).toBe("keyword");
    expect(coerceSemanticMode(null)).toBe("keyword");
    expect(coerceSemanticMode(undefined)).toBe("keyword");
    expect(coerceSemanticMode(50)).toBe("keyword");
    expect(coerceSemanticMode({ mode: "hybrid" })).toBe("keyword");
  });
});

describe("semanticModeOfWeightPercent — 実行重み→モード", () => {
  it("3値はそのまま対応 (0=keyword / 50=hybrid / 100=semantic)", () => {
    expect(semanticModeOfWeightPercent(0)).toBe("keyword");
    expect(semanticModeOfWeightPercent(50)).toBe("hybrid");
    expect(semanticModeOfWeightPercent(100)).toBe("semantic");
  });

  it("連続値・境界値は3モードへ落とす (0<w<100=hybrid)", () => {
    expect(semanticModeOfWeightPercent(1)).toBe("hybrid");
    expect(semanticModeOfWeightPercent(63)).toBe("hybrid");
    expect(semanticModeOfWeightPercent(99)).toBe("hybrid");
  });
});

describe("semanticModeLabel — モードのUIラベル", () => {
  it("キーワード / ハイブリッド / 意味", () => {
    expect(semanticModeLabel("keyword")).toBe("キーワード");
    expect(semanticModeLabel("hybrid")).toBe("ハイブリッド");
    expect(semanticModeLabel("semantic")).toBe("意味");
  });
});

describe("searchBadgeLabel — 折りたたみ行の検索mode badge", () => {
  it("query検索: 0=キーワード / 50=ハイブリッド / 100=意味", () => {
    expect(searchBadgeLabel("query", 0)).toBe("キーワード");
    expect(searchBadgeLabel("query", 50)).toBe("ハイブリッド");
    expect(searchBadgeLabel("query", 100)).toBe("意味");
  });

  it("類似検索 (kind=similar) は重みによらず「類似」", () => {
    expect(searchBadgeLabel("similar", 100)).toBe("類似");
    expect(searchBadgeLabel("similar", 0)).toBe("類似");
  });
});

describe("SemanticModeStore — localStorage永続化", () => {
  it("save→loadの往復 (キー gento.search.semanticMode)", () => {
    const storage = memoryStorage();
    const store = new SemanticModeStore(storage);
    store.save("hybrid");
    expect(storage.getItem(SEMANTIC_MODE_STORAGE_KEY)).toBe("hybrid");
    expect(store.load()).toBe("hybrid");
  });

  it("未保存・破損値のloadはkeywordへフォールバック", () => {
    expect(new SemanticModeStore(memoryStorage()).load()).toBe("keyword");
    expect(new SemanticModeStore(memoryStorage({
      [SEMANTIC_MODE_STORAGE_KEY]: "broken",
    })).load()).toBe("keyword");
  });
});
