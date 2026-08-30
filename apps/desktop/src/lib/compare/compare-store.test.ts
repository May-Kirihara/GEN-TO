import { describe, expect, it } from "vitest";
import type { ImageListItem } from "../catalog-types";
import { CompareStore, type CompareIpc } from "./compare-store";

type InvokeCall = { command: string; args?: Record<string, unknown> };

function makeItems(count: number): ImageListItem[] {
  return Array.from({ length: count }, (_, i) => ({
    id: String(i + 1),
    displayName: `item-${i}`,
    width: i % 2 === 0 ? 1600 : 1000,
    height: i % 2 === 0 ? 1000 : 1200,
    byteSize: 1000,
    state: "ready" as const,
    thumbnailState: "ready" as const,
    version: 200_000 + i,
  }));
}

function makeIpc() {
  const calls: InvokeCall[] = [];
  const handlers = new Map<string, (event: { payload: unknown }) => void>();
  const ipc: CompareIpc = {
    invoke: async <T>(command: string, args?: Record<string, unknown>): Promise<T> => {
      calls.push({ command, args });
      return {} as T;
    },
    listen: async <T>(
      event: string,
      handler: (event: { payload: T }) => void,
    ): Promise<() => void> => {
      handlers.set(event, handler as (event: { payload: unknown }) => void);
      return () => handlers.delete(event);
    },
  };
  return {
    ipc,
    calls,
    emit(event: string, payload: unknown) {
      handlers.get(event)?.({ payload });
    },
  };
}

function makeStore(items: ImageListItem[], mock: ReturnType<typeof makeIpc>): CompareStore {
  return new CompareStore(mock.ipc, {
    total: () => items.length,
    get: (index) => items[index] ?? null,
    ensureRange: async () => {},
    findIndexById: (id) => {
      const index = items.findIndex((item) => item.id === id);
      return index >= 0 ? index : null;
    },
  });
}

function emitReady(mock: ReturnType<typeof makeIpc>, id: string): void {
  mock.emit("thumb:ready", {
    items: [
      { asset_id: id, size_class: "preview2048", cache_generation: 1, version: 7 },
    ],
  });
}

describe("CompareStore: 充填とグループ", () => {
  it("選択2件でスロットが充填され、未ready分はinteractive要求される", async () => {
    const mock = makeIpc();
    const store = makeStore(makeItems(10), mock);
    await store.init();
    await store.open(2, ["3", "5"], 0);
    expect(store.slots.map((s) => s.assetId)).toEqual(["3", "5"]);
    expect(store.slots[0]?.url).toBeNull();
    const requests = mock.calls.filter((c) => c.command === "request_thumbnails");
    expect(requests.map((r) => r.args?.ids)).toEqual([["3"], ["5"]]);
    expect(requests.every((r) => r.args?.priority === "interactive")).toBe(true);
    emitReady(mock, "3");
    emitReady(mock, "5");
    expect(store.slots[0]?.url).toContain("gento-preview://");
    expect(store.slots[1]?.url).toContain("gento-preview://");
  });

  it("次グループで基準スロットのassetIdは不変", async () => {
    const mock = makeIpc();
    const store = makeStore(makeItems(10), mock);
    await store.init();
    await store.open(4, ["1", "2", "3", "4"], 0);
    store.setBase(1);
    await store.nextGroup();
    expect(store.slots[1]?.assetId).toBe("2");
    expect(store.slots.map((s) => s.assetId)).toEqual(["5", "2", "6", "7"]);
    expect(store.stats.groups).toBe(1);
  });

  it("swapは2枚の左右を入れ替える", async () => {
    const mock = makeIpc();
    const store = makeStore(makeItems(4), mock);
    await store.init();
    await store.open(2, ["1", "2"], 0);
    store.swap();
    expect(store.slots.map((s) => s.assetId)).toEqual(["2", "1"]);
    expect(store.stats.swaps).toBe(1);
  });

  it("2⇔4切替で既存スロットを保持しつつ追加/削除する", async () => {
    const mock = makeIpc();
    const store = makeStore(makeItems(8), mock);
    await store.init();
    await store.open(2, ["1", "2"], 0);
    await store.setCount(4);
    expect(store.slots.map((s) => s.assetId)).toEqual(["1", "2", "3", "4"]);
    await store.setCount(2);
    expect(store.slots.map((s) => s.assetId)).toEqual(["1", "2"]);
  });
});

describe("CompareStore: 同期", () => {
  async function openedStore() {
    const mock = makeIpc();
    const store = makeStore(makeItems(10), mock);
    await store.init();
    await store.open(2, ["1", "2"], 0);
    for (const id of ["1", "2"]) {
      emitReady(mock, id);
    }
    store.setSlotViewport(0, 800, 600);
    store.setSlotViewport(1, 800, 600);
    return { store, mock };
  }

  it("sync ONのパンで全スロットの有効centerが同じ相対位置へ変化する", async () => {
    const { store } = await openedStore();
    expect(store.syncEnabled).toBe(true);
    const before0 = store.effectiveTransform(0);
    const before1 = store.effectiveTransform(1);
    store.dragBy(0, 160, -100);
    const after0 = store.effectiveTransform(0);
    const after1 = store.effectiveTransform(1);
    // slot0: 1600x1000 (fit 0.5, scaledW 800) → dx=160 → -0.2
    expect(after0.centerX).toBeCloseTo(before0.centerX - 0.2, 9);
    expect(after1.centerX).toBeCloseTo(before1.centerX - 0.2, 9);
    expect(after0.centerY).toBeCloseTo(before0.centerY + 100 / 500, 9);
    expect(after1.centerY).toBeCloseTo(before1.centerY + 100 / 500, 9);
  });

  it("detachedスロットは共有更新の影響を受けない", async () => {
    const { store } = await openedStore();
    store.toggleDetached(1);
    const own1 = store.effectiveTransform(1);
    store.dragBy(0, 160, 0);
    expect(store.effectiveTransform(1)).toEqual(own1);
    expect(store.effectiveTransform(1).centerX).toBeCloseTo(0.5, 9);
    // 再接続すると共有へ追従する
    store.toggleDetached(1);
    expect(store.effectiveTransform(1).centerX).toBeCloseTo(store.sync.centerX, 9);
  });

  it("sync OFF中のパン位置は同期再ONでアクティブスロット基準として採用される", async () => {
    const { store } = await openedStore();
    store.toggleSync(); // OFF
    store.dragBy(0, 160, -100); // slot0のみ移動 (own0が変化、共有状態は不変)
    const panned = store.effectiveTransform(0);
    expect(panned.centerX).toBeCloseTo(0.3, 9);
    store.toggleSync(); // ON
    expect(store.syncEnabled).toBe(true);
    // slot0の表示位置は維持される (新しい共有状態がown0を採用)
    expect(store.effectiveTransform(0).centerX).toBeCloseTo(panned.centerX, 9);
    expect(store.effectiveTransform(0).centerY).toBeCloseTo(panned.centerY, 9);
    expect(store.sync.centerX).toBeCloseTo(panned.centerX, 9);
    // zoomFactorもown0のscaleから再構築される
    expect(store.sync.zoomFactor).toBeCloseTo(panned.scale / 0.5, 9);
  });

  it("sync OFF中のfitは全スロットをfitへ戻す (共有リセットが見えない状態を作らない)", async () => {
    const { store } = await openedStore();
    store.toggleSync(); // OFF
    store.dragBy(1, 0, 120); // slot1のみ移動
    expect(store.effectiveTransform(1).centerY).not.toBeCloseTo(0.5, 9);
    store.setView("fit");
    for (let i = 0; i < store.slots.length; i++) {
      const t = store.effectiveTransform(i);
      expect(t.centerX).toBeCloseTo(0.5, 9);
      expect(t.centerY).toBeCloseTo(0.5, 9);
      expect(t.mode).toBe("fit");
    }
  });

  it("sync OFFでは操作スロットのみ更新される", async () => {
    const { store } = await openedStore();
    store.toggleSync();
    expect(store.syncEnabled).toBe(false);
    const before1 = store.effectiveTransform(1);
    store.dragBy(0, 160, 0);
    expect(store.effectiveTransform(0).centerX).toBeCloseTo(0.3, 9);
    expect(store.effectiveTransform(1)).toEqual(before1);
  });

  it("sync ONのホイールは共有zoomFactorを更新する", async () => {
    const { store } = await openedStore();
    store.wheelZoom(1, { x: 400, y: 300 }, 1.5);
    expect(store.sync.zoomFactor).toBeCloseTo(1.5, 9);
    expect(store.effectiveTransform(0).scale).toBeCloseTo(
      1.5 * 0.5,
      9,
    ); // slot0 fit=0.5
  });

  it("detachedスロットへのホイールはownのみ更新する", async () => {
    const { store } = await openedStore();
    store.toggleDetached(0);
    store.wheelZoom(0, { x: 400, y: 300 }, 2);
    expect(store.sync.zoomFactor).toBeCloseTo(1, 9);
    expect(store.effectiveTransform(0).scale).toBeCloseTo(1.0, 9); // fit 0.5 × 2
  });
});

describe("CompareStore: 昇格と保持", () => {
  it("アクティブスロットのみoriginalへ昇格し、解除でpreviewへ戻る", async () => {
    const mock = makeIpc();
    const store = makeStore(makeItems(10), mock);
    await store.init();
    await store.open(2, ["1", "2"], 0);
    emitReady(mock, "1");
    emitReady(mock, "2");
    store.setActive(1);
    store.promote(true);
    expect(store.slots[1]?.isOriginal).toBe(true);
    expect(store.slots[1]?.url).toContain("gento-original://");
    expect(store.slots[0]?.isOriginal).toBe(false);
    expect(store.slots[1]?.retainedUrl).toContain("gento-preview://");
    store.promote(false);
    expect(store.slots[1]?.isOriginal).toBe(false);
    expect(store.slots[1]?.url).toContain("gento-preview://");
  });

  it("onloadで保持レイヤーが解除される", async () => {
    const mock = makeIpc();
    const store = makeStore(makeItems(10), mock);
    await store.init();
    await store.open(2, ["1", "2"], 0);
    emitReady(mock, "1");
    emitReady(mock, "2");
    store.setActive(0);
    store.promote(true);
    expect(store.slots[0]?.retainedUrl).not.toBeNull();
    store.noteLoaded(0, store.slots[0]!.key);
    expect(store.slots[0]?.retainedUrl).toBeNull();
  });

  it("originalロード失敗でpreviewへ復帰する", async () => {
    const mock = makeIpc();
    const store = makeStore(makeItems(10), mock);
    await store.init();
    await store.open(2, ["1", "2"], 0);
    emitReady(mock, "1");
    store.setActive(0);
    store.promote(true);
    store.noteLoadError(0, store.slots[0]!.key);
    expect(store.slots[0]?.isOriginal).toBe(false);
    expect(store.slots[0]?.url).toContain("gento-preview://");
  });
});
