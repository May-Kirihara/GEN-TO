import { describe, expect, it } from "vitest";
import type { ImageListItem } from "../catalog-types";
import type { ThumbFailedItem, ThumbReadyItem } from "../thumbnail-types";
import { ViewerStore, type ViewerIpc } from "./viewer-store";

type InvokeCall = { command: string; args?: Record<string, unknown> };

function item(index: number, overrides: Partial<ImageListItem> = {}): ImageListItem {
  return {
    id: String(index + 1), // 1始まりでindexと区別
    displayName: `item-${index}`,
    width: 1600,
    height: 1000,
    byteSize: 1000,
    state: "ready",
    thumbnailState: "ready",
    ...overrides,
  };
}

function makeItems(count: number): ImageListItem[] {
  return Array.from({ length: count }, (_, i) => item(i));
}

type IpcMock = {
  ipc: ViewerIpc;
  calls: InvokeCall[];
  emit(event: string, payload: unknown): void;
};

function makeIpc(): IpcMock {
  const calls: InvokeCall[] = [];
  const handlers = new Map<string, (event: { payload: unknown }) => void>();
  const ipc: ViewerIpc = {
    invoke: async <T>(command: string, args?: Record<string, unknown>): Promise<T> => {
      calls.push({ command, args });
      return { queued: 0 } as T;
    },
    listen: async <T>(
      event: string,
      handler: (event: { payload: T }) => void,
    ): Promise<() => void> => {
      handlers.set(event, handler as (event: { payload: unknown }) => void);
      return () => {
        handlers.delete(event);
      };
    },
  };
  return {
    ipc,
    calls,
    emit(event, payload) {
      handlers.get(event)?.({ payload });
    },
  };
}

function readyPayload(id: string, gen = 1): { items: ThumbReadyItem[] } {
  return { items: [{ asset_id: id, size_class: "preview2048", cache_generation: gen }] };
}

function makeStore(items: ImageListItem[], mock: IpcMock, now: () => number): ViewerStore {
  return new ViewerStore(mock.ipc, {
    total: () => items.length,
    get: (index) => items[index] ?? null,
    ensureRange: async () => {},
  }, now);
}

describe("ViewerStore: ナビゲーションと世代ID", () => {
  it("open→onload で適用され、retainedは解除される", async () => {
    const mock = makeIpc();
    let clock = 0;
    const store = makeStore(makeItems(5), mock, () => clock);
    await store.init();
    // preview2048をreadyにしておく
    mock.emit("thumb:ready", readyPayload("1"));
    await store.open(0);
    expect(store.current?.assetId).toBe("1");
    expect(store.current?.url).toContain("gento-preview://");
    clock = 50;
    store.noteLoaded(store.current!.key);
    expect(store.retained).toBeNull();
    expect(store.stats.applied).toBe(1);
    expect(store.prepOn.stats().count).toBe(1); // 既定で先読みON
  });

  it("急速ナビゲーションでは最後のonloadのみ適用される", async () => {
    const mock = makeIpc();
    let clock = 0;
    const store = makeStore(makeItems(5), mock, () => clock);
    await store.init();
    for (const id of ["1", "2", "3"]) {
      mock.emit("thumb:ready", readyPayload(id));
    }
    await store.open(0);
    const key1 = store.current!.key;
    clock = 10;
    store.noteLoaded(key1);
    // キーリピート: 2連続ナビ (ロード完了を待たない。indexは即時切替)
    store.nav(1);
    const key2 = store.current!.key;
    store.nav(1);
    expect(store.displayIndex).toBe(2);
    expect(store.retained?.assetId).toBe("1"); // 最後に表示済みだった画像を保持
    await new Promise((resolve) => setTimeout(resolve, 0)); // 項目解決を待つ
    expect(store.current?.assetId).toBe("3");
    clock = 30;
    // 古いロード完了 (gen2) が届いても現在 (gen3) を上書きしない
    store.noteLoaded(key2);
    expect(store.current?.assetId).toBe("3");
    expect(store.retained?.assetId).toBe("1");
    expect(store.stats.applied).toBe(1);
    // 最終onloadのみ適用
    const key3 = store.current!.key;
    store.noteLoaded(key3);
    expect(store.retained).toBeNull();
    expect(store.stats.applied).toBe(2);
  });

  it("未解決 (url未設定) の現行画像は保持レイヤーを上書きしない", async () => {
    const mock = makeIpc();
    const store = makeStore(makeItems(5), mock, () => 0);
    await store.init();
    mock.emit("thumb:ready", readyPayload("1"));
    await store.open(0);
    store.noteLoaded(store.current!.key);
    // index1のpreviewは未ready → 現行はurlなし
    store.nav(1);
    expect(store.current?.url).toBeNull();
    expect(store.retained?.assetId).toBe("1");
    // さらにナビ (index2も未ready) → retainedは維持される
    store.nav(1);
    expect(store.current?.url).toBeNull();
    expect(store.retained?.assetId).toBe("1");
  });

  it("未キャッシュ時はinteractive優先度で要求しthumb:readyでURLが設定される", async () => {
    const mock = makeIpc();
    const store = makeStore(makeItems(3), mock, () => 0);
    await store.init();
    await store.open(0);
    expect(store.current?.url).toBeNull();
    const req = mock.calls.find((c) => c.command === "request_thumbnails");
    expect(req?.args).toMatchObject({
      ids: ["1"],
      sizeClass: "preview2048",
      priority: "interactive",
    });
    mock.emit("thumb:ready", readyPayload("1"));
    expect(store.current?.url).toContain("gento-preview://");
  });

  it("表示可能でない項目はnote付きでurlを設定しない", async () => {
    const mock = makeIpc();
    const items = [item(0, { state: "corrupt", width: null, height: null })];
    const store = makeStore(items, mock, () => 0);
    await store.init();
    await store.open(0);
    expect(store.current?.url).toBeNull();
    expect(store.current?.note).toContain("corrupt");
  });
});

describe("ViewerStore: 先読み", () => {
  it("readyな隣接indexはウォーム対象、未readyはprefetch優先度で要求される", async () => {
    const mock = makeIpc();
    const store = makeStore(makeItems(10), mock, () => 0);
    await store.init();
    store.prefetchCount = 2;
    mock.emit("thumb:ready", readyPayload("3")); // index2のみready
    mock.calls.length = 0;
    await store.open(0); // index0 (id "1") は未ready
    // index0: interactive、index1 (id "2", 未ready): prefetch優先度
    const requests = mock.calls.filter((c) => c.command === "request_thumbnails");
    expect(requests).toHaveLength(2);
    expect(requests[0]?.args).toMatchObject({ ids: ["1"], priority: "interactive" });
    expect(requests[1]?.args).toMatchObject({ ids: ["2"], priority: "prefetch" });
    // index2 (id "3") はready → ウォーム対象 (要求なし)
    expect(store.stats.prefetchReady).toBe(1);
    // pending中の再オープンでは重複要求しない
    await store.open(0);
    expect(mock.calls.filter((c) => c.command === "request_thumbnails")).toHaveLength(2);
  });

  it("先読み無効時は隣接要求が出ない", async () => {
    const mock = makeIpc();
    const store = makeStore(makeItems(10), mock, () => 0);
    await store.init();
    store.togglePrefetch();
    expect(store.prefetchEnabled).toBe(false);
    mock.calls.length = 0;
    await store.open(0);
    const requests = mock.calls.filter((c) => c.command === "request_thumbnails");
    expect(requests).toHaveLength(1);
    expect(requests[0]?.args).toMatchObject({ priority: "interactive" });
  });
});

describe("ViewerStore: 計測", () => {
  it("nav→onload時間を先読みon/off別に記録し、staleなonloadは記録しない", async () => {
    const mock = makeIpc();
    let clock = 1000;
    const store = makeStore(makeItems(10), mock, () => clock);
    await store.init();
    for (const id of ["1", "2", "3", "4"]) {
      mock.emit("thumb:ready", readyPayload(id));
    }
    await store.open(0);
    clock = 1080;
    store.noteLoaded(store.current!.key);
    store.nav(1);
    const staleKey = store.current!.key;
    store.nav(1); // さらに進む → staleKeyのonloadは破棄
    clock = 1200;
    store.noteLoaded(staleKey); // 古い世代 → 適用・記録されない
    expect(store.stats.applied).toBe(1);
    store.noteLoaded(store.current!.key);
    expect(store.stats.applied).toBe(2);
    expect(store.prepOn.stats()).toEqual({ count: 2, p50: 80, p95: 120, p99: 120 });
    // 先読みoffでリセットして別バケットへ
    store.togglePrefetch();
    store.resetMetrics();
    expect(store.prepOn.stats().count).toBe(0);
    store.nav(1);
    clock = 1210;
    store.noteLoaded(store.current!.key);
    expect(store.prepOff.stats().count).toBe(1);
    expect(store.prepOn.stats().count).toBe(0);
  });
});

describe("ViewerStore: original昇格とtransform", () => {
  it("actual時はoriginalUrlへ差し替え、解除でpreviewUrlへ戻る", async () => {
    const mock = makeIpc();
    const store = makeStore(makeItems(3), mock, () => 0);
    await store.init();
    mock.emit("thumb:ready", readyPayload("1"));
    await store.open(0);
    store.setMode("actual");
    expect(store.current?.isOriginal).toBe(true);
    expect(store.current?.url).toContain("gento-original://");
    expect(store.retained?.url).toContain("gento-preview://"); // 旧 (preview) を保持
    store.setMode("fit");
    expect(store.current?.isOriginal).toBe(false);
    expect(store.current?.url).toContain("gento-preview://");
  });

  it("originalのロード失敗はpreviewへ復帰する", async () => {
    const mock = makeIpc();
    const store = makeStore(makeItems(3), mock, () => 0);
    await store.init();
    mock.emit("thumb:ready", readyPayload("1"));
    await store.open(0);
    store.setMode("actual");
    const key = store.current!.key;
    store.noteLoadError(key);
    expect(store.current?.isOriginal).toBe(false);
    expect(store.current?.url).toContain("gento-preview://");
  });

  it("ナビゲーションでfitにリセットされ、zoom/panでcustomになる", async () => {
    const mock = makeIpc();
    const store = makeStore(makeItems(5), mock, () => 0);
    await store.init();
    mock.emit("thumb:ready", readyPayload("1"));
    await store.open(0);
    store.setViewport(800, 600);
    expect(store.transform.mode).toBe("fit");
    store.zoomAt({ x: 400, y: 300 }, 2);
    expect(store.transform.mode).toBe("custom");
    expect(store.transform.scale).toBeCloseTo(1, 6); // fit=0.5→wait 800x600/1600x1000 → 0.5
    store.setMode("fit");
    expect(store.transform.mode).toBe("fit");
    expect(store.transform.centerX).toBeCloseTo(0.5, 10);
    // ビューポート変更でfitは追従する
    store.setViewport(400, 300);
    expect(store.transform.scale).toBeCloseTo(0.25, 10);
  });

  it("preview未readyのうちにactualへ切替えると、ready到着後にoriginalへ段階昇格する", async () => {
    const mock = makeIpc();
    const store = makeStore(makeItems(3), mock, () => 0);
    await store.init();
    await store.open(0); // 未ready → interactive要求のみ
    store.setMode("actual"); // url未設定のため昇格見送り、transformのみactual
    expect(store.current?.isOriginal).toBe(false);
    expect(store.transform.mode).toBe("actual");
    mock.emit("thumb:ready", readyPayload("1"));
    // actual表示が要求されているため、preview到着を保持レイヤーへ回してoriginalへ差し替える
    expect(store.current?.isOriginal).toBe(true);
    expect(store.current?.url).toContain("gento-original://");
    expect(store.retained?.url).toContain("gento-preview://");
  });

  it("要求世代ID (generation) はナビゲーションごとに増加する", async () => {
    const mock = makeIpc();
    const store = makeStore(makeItems(5), mock, () => 0);
    await store.init();
    const before = store.generation;
    store.nav(1);
    store.nav(1);
    expect(store.generation).toBe(before + 2);
  });

  it("actual→custom→fitでもactual自動昇格分はpreviewへ復帰する (明示昇格はfitでも維持)", async () => {
    const mock = makeIpc();
    const store = makeStore(makeItems(3), mock, () => 0);
    await store.init();
    mock.emit("thumb:ready", readyPayload("1"));
    await store.open(0);
    store.setMode("actual"); // 自動昇格
    expect(store.current?.isOriginal).toBe(true);
    store.zoomAt({ x: 400, y: 300 }, 1.5); // custom (ズーム中はoriginal維持)
    expect(store.transform.mode).toBe("custom");
    expect(store.current?.isOriginal).toBe(true);
    store.setMode("fit");
    expect(store.current?.isOriginal).toBe(false); // 自動昇格分は復帰
    expect(store.current?.url).toContain("gento-preview://");
    // 明示的なoriginal昇格はfitへ戻しても維持される
    store.setOriginal(true);
    store.setMode("fit");
    expect(store.current?.isOriginal).toBe(true);
    // ナビゲーションで昇格状態は持ち越されない
    mock.emit("thumb:ready", readyPayload("2"));
    store.nav(1);
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(store.current?.isOriginal).toBe(false);
  });

  it("thumb:failed でpendingが解除され、再ナビで再要求される", async () => {
    const mock = makeIpc();
    const store = makeStore(makeItems(3), mock, () => 0);
    await store.init();
    await store.open(0);
    // index0: interactive + index1の先読み (prefetch) = 2要求
    expect(mock.calls.filter((c) => c.command === "request_thumbnails")).toHaveLength(2);
    mock.emit("thumb:failed", {
      items: [{ asset_id: "1", size_class: "preview2048", reason: "decode" } satisfies ThumbFailedItem],
    });
    await store.open(0); // 再オープンで失敗分のみ再要求 (先読み分はpendingのまま)
    const requests = mock.calls.filter((c) => c.command === "request_thumbnails");
    expect(requests).toHaveLength(3);
    expect(requests[2]?.args).toMatchObject({ ids: ["1"], priority: "interactive" });
  });
});
