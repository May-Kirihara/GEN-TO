import { describe, expect, it } from "vitest";
import {
  DEFAULT_LAYOUT_PREFS,
  LayoutPrefsStore,
  PANE_MAX_WIDTH,
  PANE_MIN_WIDTH,
  clampPaneWidth,
  parseLayoutPrefs,
} from "./layout-prefs";

type StorageLike = { getItem(k: string): string | null; setItem(k: string, v: string): void };

function makeStorage(initial = new Map<string, string>()): StorageLike & { data: Map<string, string> } {
  return {
    data: initial,
    getItem: (k) => initial.get(k) ?? null,
    setItem: (k, v) => {
      initial.set(k, v);
    },
  };
}

describe("LayoutPrefsStore", () => {
  it("未保存時は既定値", () => {
    const store = new LayoutPrefsStore(makeStorage());
    expect(store.load()).toEqual(DEFAULT_LAYOUT_PREFS);
  });

  it("保存→復元の往復", () => {
    const storage = makeStorage();
    const store = new LayoutPrefsStore(storage);
    store.save({
      leftVisible: false,
      rightVisible: true,
      leftWidth: 320,
      rightWidth: 410,
      thumbSize: "s512",
      scrollSpeed: 3.2,
      leftTab: "collections",
      searchExpanded: false,
    });
    expect(store.load()).toEqual({
      leftVisible: false,
      rightVisible: true,
      leftWidth: 320,
      rightWidth: 410,
      thumbSize: "s512",
      scrollSpeed: 3.2,
      leftTab: "collections",
      searchExpanded: false,
    });
  });

  it("破損JSON・欠損フィールドは既定値へフォールバック", () => {
    expect(parseLayoutPrefs("{oops")).toBeNull();
    const partial = parseLayoutPrefs('{"leftVisible":false}');
    expect(partial).toEqual({
      leftVisible: false,
      rightVisible: DEFAULT_LAYOUT_PREFS.rightVisible,
      leftWidth: DEFAULT_LAYOUT_PREFS.leftWidth,
      rightWidth: DEFAULT_LAYOUT_PREFS.rightWidth,
      thumbSize: DEFAULT_LAYOUT_PREFS.thumbSize,
      scrollSpeed: DEFAULT_LAYOUT_PREFS.scrollSpeed,
      leftTab: DEFAULT_LAYOUT_PREFS.leftTab,
      searchExpanded: DEFAULT_LAYOUT_PREFS.searchExpanded,
    });
    // 未知のサムネイルサイズは既定へ (前方互換)
    const future = parseLayoutPrefs('{"thumbSize":"s1024"}');
    expect(future?.thumbSize).toBe("s256");
  });

  it("幅は保存時・復元時ともクランプされる", () => {
    expect(clampPaneWidth(10)).toBe(PANE_MIN_WIDTH);
    expect(clampPaneWidth(9999)).toBe(PANE_MAX_WIDTH);
    expect(clampPaneWidth(Number.NaN)).toBe(DEFAULT_LAYOUT_PREFS.leftWidth);
    const parsed = parseLayoutPrefs('{"leftWidth":1,"rightWidth":99999}');
    expect(parsed?.leftWidth).toBe(PANE_MIN_WIDTH);
    expect(parsed?.rightWidth).toBe(PANE_MAX_WIDTH);
  });

  describe("scrollSpeed (UIUX-1 / UIUX-3 既定2.0)", () => {
    it("未保存・欠損時は既定2.0 (UIUX-3で1.0→2.0へ変更)", () => {
      expect(DEFAULT_LAYOUT_PREFS.scrollSpeed).toBe(2);
      expect(parseLayoutPrefs("{}")?.scrollSpeed).toBe(2);
      expect(parseLayoutPrefs('{"scrollSpeed":"x"}')?.scrollSpeed).toBe(2);
    });

    it("保存済み1.0は保持 (no-migration: ユーザー明示選択を壊さない)", () => {
      expect(parseLayoutPrefs('{"scrollSpeed":1}')?.scrollSpeed).toBe(1);
    });

    it("保存→復元の往復", () => {
      const storage = makeStorage();
      const store = new LayoutPrefsStore(storage);
      store.save({ ...DEFAULT_LAYOUT_PREFS, scrollSpeed: 2.5 });
      expect(store.load().scrollSpeed).toBe(2.5);
    });

    it("範囲クランプ (0.5–5.0) と0.1刻みへの丸め", () => {
      const clamped = parseLayoutPrefs(
        '{"scrollSpeed":0.1}',
      );
      expect(clamped?.scrollSpeed).toBe(0.5);
      expect(parseLayoutPrefs('{"scrollSpeed":99}')?.scrollSpeed).toBe(5);
      // 0.1刻みに丸め (浮動小数の桁落ちを吸収)
      expect(parseLayoutPrefs('{"scrollSpeed":1.25000000001}')?.scrollSpeed).toBe(1.3);
      expect(parseLayoutPrefs('{"scrollSpeed":1.24999}')?.scrollSpeed).toBe(1.2);
    });
  });

  describe("leftTab (UIUX-1: 左ペインタブ)", () => {
    it("未保存・欠損・不正値は既定folders", () => {
      expect(DEFAULT_LAYOUT_PREFS.leftTab).toBe("folders");
      expect(parseLayoutPrefs("{}")?.leftTab).toBe("folders");
      expect(parseLayoutPrefs('{"leftTab":"download"}')?.leftTab).toBe("folders");
    });

    it("保存→復元の往復", () => {
      const storage = makeStorage();
      const store = new LayoutPrefsStore(storage);
      store.save({ ...DEFAULT_LAYOUT_PREFS, leftTab: "library" });
      expect(store.load().leftTab).toBe("library");
      // 旧バージョン保存値 (leftTab無し) との前方互換: 既定へ
      expect(parseLayoutPrefs('{"leftVisible":true}')?.leftTab).toBe("folders");
    });
  });

  describe("searchExpanded (UIUX-3: 検索行の折りたたみ)", () => {
    it("未保存・欠損・不正値は既定true (展開)", () => {
      expect(DEFAULT_LAYOUT_PREFS.searchExpanded).toBe(true);
      expect(parseLayoutPrefs("{}")?.searchExpanded).toBe(true);
      expect(parseLayoutPrefs('{"searchExpanded":"yes"}')?.searchExpanded).toBe(true);
    });

    it("折りたたみ (false) の保存→復元の往復", () => {
      const storage = makeStorage();
      const store = new LayoutPrefsStore(storage);
      store.save({ ...DEFAULT_LAYOUT_PREFS, searchExpanded: false });
      expect(store.load().searchExpanded).toBe(false);
      expect(parseLayoutPrefs('{"searchExpanded":false}')?.searchExpanded).toBe(false);
    });
  });
});
