import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  SESSION_STORAGE_KEY,
  SelectionSessionStore,
  generateSessionId,
  parsePersistedSession,
  planAdhocResume,
  sessionCountDrifted,
  type PersistedSession,
} from "./selection-session";

type StorageLike = {
  getItem(k: string): string | null;
  setItem(k: string, v: string): void;
  removeItem(k: string): void;
};

function makeStorage(initial = new Map<string, string>()): StorageLike & { data: Map<string, string> } {
  return {
    data: initial,
    getItem: (k) => initial.get(k) ?? null,
    setItem: (k, v) => {
      initial.set(k, v);
    },
    removeItem: (k) => {
      initial.delete(k);
    },
  };
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("SelectionSessionStore", () => {
  it("開始で順序付き列・位置0・確認済み1で始まる", () => {
    const store = new SelectionSessionStore(makeStorage());
    store.start(["a", "b", "c"], "フォルダ /x");
    expect(store.active).toBe(true);
    const counts = store.counts(() => "undecided");
    expect(counts).toEqual({
      total: 3,
      position: 0,
      confirmed: 1,
      keep: 0,
      reject: 0,
      undecided: 3,
    });
    expect(store.session?.origin).toBe("フォルダ /x");
    expect(store.session?.id).toMatch(/^[-0-9a-f]{10,}$/); // uuid形
  });

  it("空の対象では開始できずlastError", () => {
    const store = new SelectionSessionStore(makeStorage());
    store.start([], "空");
    expect(store.active).toBe(false);
    expect(store.lastError).toContain("空");
  });

  it("next/prevで移動し確認済みが増える (端では止まる)", () => {
    const store = new SelectionSessionStore(makeStorage());
    store.start(["a", "b", "c"], "test");
    store.next();
    store.next();
    store.next(); // 位置2で停止
    expect(store.session?.position).toBe(2);
    store.next();
    expect(store.session?.position).toBe(2);
    store.prev();
    expect(store.session?.position).toBe(1);
    expect(store.session?.visited.length).toBe(3);
    expect(store.currentId()).toBe("b");
  });

  it("noteFocusはセッション内IDだけ位置更新 (グリッド選択同期)", () => {
    const store = new SelectionSessionStore(makeStorage());
    store.start(["a", "b", "c"], "test");
    store.noteFocus("c");
    expect(store.session?.position).toBe(2);
    store.noteFocus("zzz"); // セッション外 → 無視
    expect(store.session?.position).toBe(2);
  });

  it("countsはtriage集計 (採用/除外/保留)", () => {
    const store = new SelectionSessionStore(makeStorage());
    store.start(["a", "b", "c", "d"], "test");
    const triage = (id: string) =>
      id === "a" ? "keep" : id === "b" ? "reject" : id === "c" ? "keep" : "undecided";
    expect(store.counts(triage)).toEqual({
      total: 4,
      position: 0,
      confirmed: 1,
      keep: 2,
      reject: 1,
      undecided: 1,
    });
  });

  it("位置移動は遅延保存され、再開で復元される (uuidセッション名)", () => {
    const storage = makeStorage();
    const store = new SelectionSessionStore(storage);
    store.start(["a", "b", "c"], "フォルダ /p");
    store.next();
    expect(storage.data.has(SESSION_STORAGE_KEY)).toBe(false); // デバウンス中
    vi.advanceTimersByTime(400);
    const raw = storage.data.get(SESSION_STORAGE_KEY) ?? "";
    expect(raw).toContain('"origin":"フォルダ /p"');
    const parsed = parsePersistedSession(raw);
    expect(parsed?.position).toBe(1);
    expect(parsed?.visited).toEqual([0, 1]);

    // 別インスタンス (再起動) で再開
    const restored = new SelectionSessionStore(storage);
    expect(restored.peekPersisted()).toEqual({
      origin: "フォルダ /p",
      count: 3,
      updatedAt: expect.any(Number),
    });
    expect(restored.resume()).toBe(true);
    expect(restored.session?.position).toBe(1);
    expect(restored.positionOf("c")).toBe(2);
    expect(restored.positionOf("none")).toBeNull();
  });

  it("endで永続化も削除される", () => {
    const storage = makeStorage();
    const store = new SelectionSessionStore(storage);
    store.start(["a"], "test");
    vi.advanceTimersByTime(400);
    expect(storage.data.has(SESSION_STORAGE_KEY)).toBe(true);
    store.end();
    expect(store.active).toBe(false);
    expect(storage.data.has(SESSION_STORAGE_KEY)).toBe(false);
    expect(store.counts(() => "undecided")).toBeNull();
  });

  it("破損・バージョン不合の永続化データは再開不可", () => {
    const storage = makeStorage(new Map([[SESSION_STORAGE_KEY, "{broken"]]));
    const store = new SelectionSessionStore(storage);
    expect(store.resume()).toBe(false);
    expect(parsePersistedSession('{"v":99,"id":"x"}')).toBeNull();
    // 範囲外position・visitedはクランプ/除外される
    const clamped = parsePersistedSession(
      '{"v":1,"id":"s","ids":["a","b"],"position":9,"visited":[0,7],"origin":"o","updatedAt":1}',
    );
    expect(clamped?.position).toBe(1);
    expect(clamped?.visited).toEqual([0]);
  });

  it("flushで保留中の保存を確定", () => {
    const storage = makeStorage();
    const store = new SelectionSessionStore(storage);
    store.start(["a"], "test");
    store.flush();
    expect(storage.data.has(SESSION_STORAGE_KEY)).toBe(true);
  });

  it("generateSessionIdは一意", () => {
    const ids = new Set(Array.from({ length: 100 }, () => generateSessionId()));
    expect(ids.size).toBe(100);
  });

  // ===== origin詳細と再開判定 (QA-WP4 Major-2) =====

  it("startのdetailが永続化され、parseで復元される", () => {
    const storage = makeStorage();
    const store = new SelectionSessionStore(storage);
    store.start(
      ["a", "b"],
      "/pics (2件)",
      0,
      { kind: "adhoc-folder", folderPath: "/pics", sort: "date_desc" },
    );
    store.flush();
    const parsed = parsePersistedSession(storage.data.get(SESSION_STORAGE_KEY) ?? "");
    expect(parsed?.detail).toEqual({
      kind: "adhoc-folder",
      folderPath: "/pics",
      sort: "date_desc",
    });
    // peekPersistedSessionで全体を取得できる
    const peeked = new SelectionSessionStore(storage).peekPersistedSession();
    expect(peeked?.ids).toEqual(["a", "b"]);
    expect(peeked?.detail?.kind).toBe("adhoc-folder");
  });

  it("detail未指定はnullで保存され、旧データ (detailなし) はnullに読める", () => {
    const legacy = parsePersistedSession(
      '{"v":1,"id":"s","ids":["a"],"position":0,"visited":[0],"origin":"o","updatedAt":1}',
    );
    expect(legacy?.detail).toBeNull();
    const storage = makeStorage();
    const store = new SelectionSessionStore(storage);
    store.start(["a"], "o");
    store.flush();
    expect(parsePersistedSession(storage.data.get(SESSION_STORAGE_KEY) ?? "")?.detail).toBeNull();
  });

  it("parse時: 不正なdetail (未知kind・空folderPath・不正sort) はnullへ", () => {
    const base = (detail: string) =>
      `{"v":1,"id":"s","ids":["a"],"position":0,"visited":[0],"origin":"o","detail":${detail},"updatedAt":1}`;
    expect(parsePersistedSession(base('{"kind":"adhoc-folder","folderPath":"/p","sort":"name"}'))?.detail?.kind).toBe("adhoc-folder");
    expect(parsePersistedSession(base('{"kind":"unknown"}'))?.detail).toBeNull();
    expect(parsePersistedSession(base('{"kind":"adhoc-folder","folderPath":"","sort":"name"}'))?.detail).toBeNull();
    expect(parsePersistedSession(base('{"kind":"adhoc-folder","folderPath":"/p","sort":"weird"}'))?.detail).toBeNull();
    expect(parsePersistedSession(base('{"kind":"library"}'))?.detail).toEqual({ kind: "library" });
  });

  describe("planAdhocResume", () => {
    const adhocSession = (sort: string): PersistedSession => ({
      v: 1,
      id: "s",
      ids: ["a", "b", "c"],
      position: 1,
      visited: [1],
      origin: "/pics (3件)",
      detail: { kind: "adhoc-folder", folderPath: "/pics", sort: sort as "name" },
      updatedAt: 1,
    });

    it("同一フォルダ・同一ソートならそのまま再開", () => {
      const plan = planAdhocResume(adhocSession("date_desc"), {
        source: "temp",
        folderPath: "/pics",
        sort: "date_desc",
      });
      expect(plan).toEqual({ action: "resume" });
    });

    it("ソートが異なれば保存済みfolder+sortを復元する", () => {
      const plan = planAdhocResume(adhocSession("date_desc"), {
        source: "temp",
        folderPath: "/pics",
        sort: "name",
      });
      expect(plan).toEqual({ action: "restore", folderPath: "/pics", sort: "date_desc" });
    });

    it("フォルダが異なれば復元する (再起動直後のname順もソート不一致で拾う)", () => {
      const other = planAdhocResume(adhocSession("date_desc"), {
        source: "temp",
        folderPath: "/other",
        sort: "date_desc",
      });
      expect(other).toEqual({ action: "restore", folderPath: "/pics", sort: "date_desc" });
      const freshBoot = planAdhocResume(adhocSession("date_desc"), {
        source: "temp",
        folderPath: "/other",
        sort: "name",
      });
      expect(freshBoot).toEqual({ action: "restore", folderPath: "/pics", sort: "date_desc" });
    });

    it("ライブラリ表示中でもadhocセッションは復元する", () => {
      const plan = planAdhocResume(adhocSession("name"), {
        source: "library",
        folderPath: null,
        sort: "name",
      });
      expect(plan).toEqual({ action: "restore", folderPath: "/pics", sort: "name" });
    });

    it("libraryセッションと旧データ (detailなし) は常にそのまま再開", () => {
      const librarySession: PersistedSession = {
        v: 1,
        id: "s",
        ids: ["1001"],
        position: 0,
        visited: [0],
        origin: "ライブラリ (1件)",
        detail: { kind: "library" },
        updatedAt: 1,
      };
      expect(
        planAdhocResume(librarySession, { source: "temp", folderPath: "/x", sort: "name" }),
      ).toEqual({ action: "resume" });
      const legacy: PersistedSession = { ...librarySession, detail: null };
      expect(
        planAdhocResume(legacy, { source: "temp", folderPath: "/x", sort: "size_desc" }),
      ).toEqual({ action: "resume" });
    });

    it("選択開始セッション (adhoc-selection) も同一フォルダ+ソートで再開", () => {
      const selection: PersistedSession = {
        v: 1,
        id: "s",
        ids: ["b", "a"],
        position: 0,
        visited: [0],
        origin: "選択画像 (2件)",
        detail: { kind: "adhoc-selection", folderPath: "/pics", sort: "name" },
        updatedAt: 1,
      };
      expect(
        planAdhocResume(selection, { source: "temp", folderPath: "/pics", sort: "name" }),
      ).toEqual({ action: "resume" });
      expect(
        planAdhocResume(selection, { source: "temp", folderPath: "/pics", sort: "date_desc" }),
      ).toEqual({ action: "restore", folderPath: "/pics", sort: "name" });
    });
  });

  describe("sessionCountDrifted", () => {
    it("5%以内のずれは許容し、超過は打ち切り判定", () => {
      expect(sessionCountDrifted(100, 100)).toBe(false);
      expect(sessionCountDrifted(100, 95)).toBe(false); // ちょうど5%は許容
      expect(sessionCountDrifted(100, 94)).toBe(true);
      expect(sessionCountDrifted(100, 106)).toBe(true); // 増加も対象
      expect(sessionCountDrifted(0, 50)).toBe(false); // 保存0件は判定不能
    });
  });

  it("abortは非活性化するが永続化を残す (再開中止。QA-WP4)", () => {
    const storage = makeStorage();
    const store = new SelectionSessionStore(storage);
    store.start(["a", "b"], "test");
    store.flush();
    expect(storage.data.has(SESSION_STORAGE_KEY)).toBe(true);

    store.resume();
    expect(store.active).toBe(true);
    store.abort();
    expect(store.active).toBe(false);
    expect(store.counts(() => "undecided")).toBeNull();
    // 永続化は削除しない (=endと違い、再開候補として残る)
    expect(storage.data.has(SESSION_STORAGE_KEY)).toBe(true);
    expect(store.peekPersisted()?.count).toBe(2);
  });
});
