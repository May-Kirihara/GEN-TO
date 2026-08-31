import { describe, expect, it, vi } from "vitest";
import type { ImageListItem } from "../catalog-types";
import { QuickPreviewStore, type PreviewIpc } from "./quick-preview";
import type { ThumbReadyItem } from "../thumbnail-types";

function item(index: number, overrides: Partial<ImageListItem> = {}): ImageListItem {
  return {
    id: String(index + 1),
    displayName: `img-${index}.png`,
    width: 100,
    height: 200,
    byteSize: 1234,
    state: "ready",
    thumbnailState: "missing",
    isAnimated: false,
    version: 7,
    path: `/x/img-${index}.png`,
    mtimeMs: 1000,
    mimeType: "image/png",
    dbState: "adhoc",
    rating: 0,
    favorite: false,
    triage: "undecided",
    ...overrides,
  };
}

function makeIpc() {
  const readyListeners: Array<(event: { payload: { items: ThumbReadyItem[] } }) => void> = [];
  const calls: Array<{ command: string; args?: Record<string, unknown> }> = [];
  const invoke = async <T,>(
    command: string,
    args?: Record<string, unknown>,
  ): Promise<T> => {
    calls.push({ command, args });
    if (command === "request_thumbnails") {
      return { queued: 1, rejected_dup: 0, cache_hits: 0 } as T;
    }
    throw new Error(`unexpected: ${command}`);
  };
  const ipc: PreviewIpc = {
    invoke,
    listen: async <T,>(
      event: string,
      handler: (event: { payload: T }) => void,
    ): Promise<() => void> => {
      if (event === "thumb:ready") {
        readyListeners.push(handler as (event: { payload: { items: ThumbReadyItem[] } }) => void);
      }
      return () => undefined;
    },
  };
  return {
    ipc,
    invoke,
    calls,
    emitReady: (items: ThumbReadyItem[]) => {
      for (const listener of readyListeners) {
        listener({ payload: { items } });
      }
    },
  };
}

function makeItems(count: number, overrides: Array<Partial<ImageListItem>> = []) {
  const map = new Map<number, ImageListItem>(
    Array.from({ length: count }, (_, i) => [i, item(i, overrides[i] ?? {})]),
  );
  return {
    total: () => count,
    get: (index: number) => map.get(index) ?? null,
    ensureRange: vi.fn(async () => undefined),
  };
}

describe("QuickPreviewStore", () => {
  it("openで項目を解決しpreview2048を要求、readyでURLが決まる", async () => {
    const { ipc, calls, emitReady } = makeIpc();
    const items = makeItems(3);
    const store = new QuickPreviewStore(ipc, items);
    await store.init();

    await store.open(1);
    expect(store.active).toBe(true);
    expect(store.item?.id).toBe("2");
    expect(store.url).toBeNull(); // 未キャッシュ
    expect(store.loading).toBe(true);
    expect(calls).toContainEqual({
      command: "request_thumbnails",
      args: { ids: ["2"], sizeClass: "preview2048", priority: "interactive" },
    });

    emitReady([
      {
        asset_id: "2",
        size_class: "preview2048",
        cache_generation: 1,
        version: 42,
      },
    ]);
    expect(store.url).toBe("gento-preview://localhost/asset/2?v=42");
    expect(store.loading).toBe(false);
  });

  it("ready済み項目は要求なしで即表示", async () => {
    const { ipc, calls, emitReady } = makeIpc();
    const items = makeItems(2);
    const store = new QuickPreviewStore(ipc, items);
    await store.init();
    emitReady([
      { asset_id: "1", size_class: "preview2048", cache_generation: 1, version: 5 },
    ]);
    calls.length = 0;

    await store.open(0);
    expect(store.url).toBe("gento-preview://localhost/asset/1?v=5");
    expect(calls).toEqual([]);
  });

  it("moveで前後移動 (端で停止) し新しい項目へ要求し直す", async () => {
    const { ipc, emitReady } = makeIpc();
    const items = makeItems(3);
    const store = new QuickPreviewStore(ipc, items);
    await store.init();
    emitReady([
      { asset_id: "2", size_class: "preview2048", cache_generation: 1, version: 42 },
    ]);
    await store.open(1);
    await store.move(1); // → index 2 (id 3)
    expect(store.item?.id).toBe("3");
    expect(store.url).toBeNull(); // 未キャッシュのため差し替え
    await store.move(1); // 端
    expect(store.index).toBe(2);
    await store.move(-2); // → 0
    expect(store.index).toBe(0);
  });

  it("アニメーション項目はoriginal直接表示 (要求なし)", async () => {
    const { ipc, calls } = makeIpc();
    const items = makeItems(1, [{ isAnimated: true }]);
    const store = new QuickPreviewStore(ipc, items);
    await store.init();
    await store.open(0);
    expect(store.url).toBe("gento-original://localhost/asset/1?v=7");
    expect(calls).toEqual([]);
  });

  it("ready以外の状態 (破損等) はURLなしで項目情報のみ", async () => {
    const { ipc } = makeIpc();
    const items = makeItems(1, [{ state: "corrupt" }]);
    const store = new QuickPreviewStore(ipc, items);
    await store.init();
    await store.open(0);
    expect(store.item?.state).toBe("corrupt");
    expect(store.url).toBeNull();
    expect(store.loading).toBe(false);
  });

  it("closeで非表示化しURLを解除", async () => {
    const { ipc, emitReady } = makeIpc();
    const items = makeItems(1);
    const store = new QuickPreviewStore(ipc, items);
    await store.init();
    emitReady([{ asset_id: "1", size_class: "preview2048", cache_generation: 1, version: 5 }]);
    await store.open(0);
    expect(store.url).not.toBeNull();
    store.close();
    expect(store.active).toBe(false);
    expect(store.url).toBeNull();
  });

  it("他assetのreadyでは現在表示を上書きしない", async () => {
    const { ipc, emitReady } = makeIpc();
    const items = makeItems(2);
    const store = new QuickPreviewStore(ipc, items);
    await store.init();
    await store.open(0);
    emitReady([{ asset_id: "2", size_class: "preview2048", cache_generation: 1, version: 9 }]);
    expect(store.url).toBeNull();
  });
});
