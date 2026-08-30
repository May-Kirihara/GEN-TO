import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  SESSION_STORAGE_KEY,
  SelectionSessionStore,
  generateSessionId,
  parsePersistedSession,
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
});
