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
    });
    expect(store.load()).toEqual({
      leftVisible: false,
      rightVisible: true,
      leftWidth: 320,
      rightWidth: 410,
      thumbSize: "s512",
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
});
