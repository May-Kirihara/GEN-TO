import { describe, expect, it } from "vitest";
import {
  DEFAULT_FOLDER_PREFS,
  FolderPrefsStore,
  MAX_EXPANDED_NODES,
  parseFolderPrefs,
} from "./folder-prefs";

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

describe("FolderPrefsStore", () => {
  it("未保存時は既定値", () => {
    expect(new FolderPrefsStore(makeStorage()).load()).toEqual(DEFAULT_FOLDER_PREFS);
  });

  it("保存→復元の往復", () => {
    const storage = makeStorage();
    const store = new FolderPrefsStore(storage);
    store.save({ lastFolder: "/home/u/pictures", expanded: ["/home/u", "/home/u/pictures"] });
    expect(store.load()).toEqual({
      lastFolder: "/home/u/pictures",
      expanded: ["/home/u", "/home/u/pictures"],
    });
  });

  it("破損JSON・型不一致は既定へ、expanded内の非文字列は除外", () => {
    expect(parseFolderPrefs("not json")).toBeNull();
    const parsed = parseFolderPrefs('{"lastFolder":42,"expanded":["/a",7,null]}');
    expect(parsed).toEqual({ lastFolder: null, expanded: ["/a"] });
  });

  it("展開ノード上限を超える分は古い順に切り捨て", () => {
    const many = Array.from({ length: MAX_EXPANDED_NODES + 50 }, (_, i) => `/d${i}`);
    const storage = makeStorage();
    new FolderPrefsStore(storage).save({ lastFolder: null, expanded: many });
    const loaded = new FolderPrefsStore(storage).load();
    expect(loaded.expanded).toHaveLength(MAX_EXPANDED_NODES);
    expect(loaded.expanded[0]).toBe(`/d${50}`);
    expect(loaded.expanded.at(-1)).toBe(`/d${MAX_EXPANDED_NODES + 49}`);
  });
});
