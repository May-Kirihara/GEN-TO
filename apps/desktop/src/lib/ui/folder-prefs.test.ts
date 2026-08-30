import { describe, expect, it } from "vitest";
import {
  DEFAULT_FOLDER_PREFS,
  FolderPrefsStore,
  MAX_EXPANDED_NODES,
  MAX_SORT_ENTRIES,
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
    store.save({ lastFolder: "/home/u/pictures", expanded: ["/home/u", "/home/u/pictures"], sorts: {} });
    expect(store.load()).toEqual({
      lastFolder: "/home/u/pictures",
      expanded: ["/home/u", "/home/u/pictures"],
      sorts: {},
    });
  });

  it("破損JSON・型不一致は既定へ、expanded内の非文字列は除外", () => {
    expect(parseFolderPrefs("not json")).toBeNull();
    const parsed = parseFolderPrefs('{"lastFolder":42,"expanded":["/a",7,null]}');
    expect(parsed).toEqual({ lastFolder: null, expanded: ["/a"], sorts: {} });
  });

  it("展開ノード上限を超える分は古い順に切り捨て", () => {
    const many = Array.from({ length: MAX_EXPANDED_NODES + 50 }, (_, i) => `/d${i}`);
    const storage = makeStorage();
    new FolderPrefsStore(storage).save({ lastFolder: null, expanded: many, sorts: {} });
    const loaded = new FolderPrefsStore(storage).load();
    expect(loaded.expanded).toHaveLength(MAX_EXPANDED_NODES);
    expect(loaded.expanded[0]).toBe(`/d${50}`);
    expect(loaded.expanded.at(-1)).toBe(`/d${MAX_EXPANDED_NODES + 49}`);
  });

  // ===== フォルダ別ソート (QA-WP4 Major-2) =====

  it("saveSortでフォルダ別ソートを保存しsortForで読める", () => {
    const storage = makeStorage();
    const store = new FolderPrefsStore(storage);
    expect(store.sortFor("/home/u/pics")).toBeUndefined();
    store.saveSort("/home/u/pics", "date_desc");
    store.saveSort("library", "size_desc");
    expect(store.sortFor("/home/u/pics")).toBe("date_desc");
    expect(store.sortFor("library")).toBe("size_desc");
    // 再保存は上書き
    store.saveSort("/home/u/pics", "name");
    expect(store.sortFor("/home/u/pics")).toBe("name");
  });

  it("sortsの保存→復元の往復 (他フィールドと共存)", () => {
    const storage = makeStorage();
    const store = new FolderPrefsStore(storage);
    store.save({ lastFolder: "/a", expanded: ["/a"], sorts: { "/a": "date_desc", library: "name" } });
    expect(store.load()).toEqual({
      lastFolder: "/a",
      expanded: ["/a"],
      sorts: { "/a": "date_desc", library: "name" },
    });
  });

  it("parse時: sorts内の非文字列値は除外・sorts不在は空オブジェクト", () => {
    const parsed = parseFolderPrefs('{"lastFolder":"/a","expanded":[],"sorts":{"/a":"date_desc","/b":7}}');
    expect(parsed?.sorts).toEqual({ "/a": "date_desc" });
    const noSorts = parseFolderPrefs('{"lastFolder":"/a","expanded":[]}');
    expect(noSorts?.sorts).toEqual({});
  });

  it("ソート保存フォルダ数上限を超える分は古い順に切り捨て", () => {
    const storage = makeStorage();
    const store = new FolderPrefsStore(storage);
    for (let i = 0; i < MAX_SORT_ENTRIES + 10; i++) {
      store.saveSort(`/f${i}`, "date_desc");
    }
    const sorts = store.load().sorts;
    expect(Object.keys(sorts)).toHaveLength(MAX_SORT_ENTRIES);
    expect(sorts["/f0"]).toBeUndefined();
    expect(sorts[`/f${MAX_SORT_ENTRIES + 9}`]).toBe("date_desc");
  });
});
