import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ScrollPositionStore } from "./scroll-position";

type StorageLike = { getItem(k: string): string | null; setItem(k: string, v: string): void };

function makeStorage(): StorageLike & { data: Map<string, string> } {
  const data = new Map<string, string>();
  return {
    data,
    getItem: (k) => data.get(k) ?? null,
    setItem: (k, v) => {
      data.set(k, v);
    },
  };
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("ScrollPositionStore", () => {
  it("スロットル窓内の連続保存は1回だけ書き込む (末尾の値で)", () => {
    const storage = makeStorage();
    const store = new ScrollPositionStore(storage, { throttleMs: 250 });
    store.saveThrottled("virtual", 100);
    store.saveThrottled("virtual", 300);
    store.saveThrottled("virtual", 520);
    expect(storage.data.size).toBe(0);
    vi.advanceTimersByTime(250);
    expect(storage.data.get("virtual")).toBe("520");
  });

  it("窓を過ぎた後の保存は再度スケジュールされる", () => {
    const storage = makeStorage();
    const store = new ScrollPositionStore(storage, { throttleMs: 250 });
    store.saveThrottled("virtual", 100);
    vi.advanceTimersByTime(250);
    store.saveThrottled("virtual", 900);
    vi.advanceTimersByTime(249);
    expect(storage.data.get("virtual")).toBe("100");
    vi.advanceTimersByTime(1);
    expect(storage.data.get("virtual")).toBe("900");
  });

  it("in-memory値は即座に読める (モード切替の復元用)", () => {
    const storage = makeStorage();
    const store = new ScrollPositionStore(storage, { throttleMs: 250 });
    store.saveThrottled("virtual", 1234);
    expect(store.peek("virtual")).toBe(1234);
    // localStorageへはまだ書かれていない
    expect(storage.data.size).toBe(0);
  });

  it("loadはin-memory優先、なければlocalStorage (数値でない場合はnull)", () => {
    const storage = makeStorage();
    storage.data.set("real", "777");
    const store = new ScrollPositionStore(storage, { throttleMs: 250 });
    expect(store.load("real")).toBe(777);
    store.saveThrottled("real", 42);
    expect(store.load("real")).toBe(42);
    storage.data.set("virtual", "not-a-number");
    const other = new ScrollPositionStore(storage, { throttleMs: 250 });
    expect(other.load("virtual")).toBeNull();
  });

  it("flushは保留中の書き込みを即時に確定させる", () => {
    const storage = makeStorage();
    const store = new ScrollPositionStore(storage, { throttleMs: 250 });
    store.saveThrottled("virtual", 321);
    store.flush();
    expect(storage.data.get("virtual")).toBe("321");
    // flush後はタイマーが発火しない
    vi.advanceTimersByTime(1000);
    expect(storage.data.size).toBe(1);
  });

  it("モード別キーは独立する", () => {
    const storage = makeStorage();
    const store = new ScrollPositionStore(storage, { throttleMs: 250 });
    store.saveThrottled("virtual", 10);
    store.saveThrottled("real", 20);
    vi.advanceTimersByTime(250);
    expect(storage.data.get("virtual")).toBe("10");
    expect(storage.data.get("real")).toBe("20");
  });
});
