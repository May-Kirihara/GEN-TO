// @vitest-environment happy-dom
/**
 * CompareView.svelte コンポーネントテスト (FIX-VIEWERNAV-2)。
 *
 * - スロット単位の採用/除外ボタン (.slot-cull): 実クリック配列
 *   (pointerdown→pointerup→click) ではslotのpointer capture対象外で
 *   onCullSlotが発火する (FIX-VIEWERNAV-1と同一bugのCompareView側修正)
 * - cullボタン経由pointerdownはドラッグを開始しない (skip後のmove/up安全)
 * - スロット面のpointerdownは従来どおりcapture開始+パン+up終了 (回帰防止)
 * を実CompareStore + 偽IPC/itemsで検証する。
 */

import { afterEach, describe, expect, it } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import CompareView from "./CompareView.svelte";
import { CompareStore, type CompareIpc } from "./compare-store";
import type { ImageListItem } from "../catalog-types";

function item(index: number): ImageListItem {
  return {
    id: String(index + 1),
    displayName: `item-${index}.png`,
    width: 1600,
    height: 1000,
    byteSize: 1000,
    state: "ready",
    thumbnailState: "ready",
    isAnimated: false,
    version: 100_000 + index,
    path: `/dataset/item-${index}.png`,
    mtimeMs: 1000 + index,
    mimeType: "image/png",
    dbState: "adhoc",
    rating: 0,
    favorite: false,
    triage: "undecided",
  };
}

function makeIpc(): CompareIpc {
  return {
    invoke: async <T>(): Promise<T> => ({}) as T,
    listen: async () => () => {},
  };
}

function makeStore(totalCount: number): CompareStore {
  const items = Array.from({ length: totalCount }, (_, i) => item(i));
  return new CompareStore(makeIpc(), {
    total: () => items.length,
    get: (index) => items[index] ?? null,
    ensureRange: async () => {},
    findIndexById: (id) => {
      const index = items.findIndex((entry) => entry.id === id);
      return index >= 0 ? index : null;
    },
  });
}

let target: HTMLElement | null = null;
let component: Record<string, unknown> | null = null;

function mountCompare(
  store: CompareStore,
  onCullSlot: (slotIndex: number, value: "picked" | "rejected") => void,
): void {
  target = document.createElement("div");
  document.body.appendChild(target);
  component = mount(CompareView, {
    target,
    props: { store, onClose: () => {}, onCullSlot },
  });
  flushSync();
}

function cullButton(slot: number, kind: "pick" | "reject"): HTMLButtonElement {
  const el = target!.querySelector<HTMLButtonElement>(
    `.slot[data-slot='${slot}'] .slot-cull .cull-${kind}`,
  );
  expect(el, `slot${slot} cull-${kind}`).not.toBeNull();
  return el!;
}

function slotEl(index: number): HTMLElement {
  const el = target!.querySelector<HTMLElement>(`.slot[data-slot='${index}']`);
  expect(el, `slot${index}`).not.toBeNull();
  return el!;
}

/** setPointerCapture呼出の記録パッチ (happy-domはcaptureのリターゲットを
 * シミュレートしないため、テスト側で実ブラウザ意味論を再現する)。 */
function patchPointerCapture(): {
  calls: Array<{ el: Element; pointerId: number }>;
  restore(): void;
} {
  const calls: Array<{ el: Element; pointerId: number }> = [];
  const original = Element.prototype.setPointerCapture;
  Element.prototype.setPointerCapture = function patched(
    this: Element,
    pointerId: number,
  ) {
    calls.push({ el: this, pointerId });
    return undefined;
  } as typeof Element.prototype.setPointerCapture;
  return { calls, restore: () => {
    Element.prototype.setPointerCapture = original;
  } };
}

/** down/up両対象の最近共通祖先 (実ブラウザのclick発火対象の決定規則)。 */
function nearestCommonAncestor(a: Node, b: Node): Node {
  const bChain = new Set<Node>();
  for (let n: Node | null = b; n !== null; n = n.parentNode) {
    bChain.add(n);
  }
  for (let n: Node | null = a; n !== null; n = n.parentNode) {
    if (bChain.has(n)) {
      return n;
    }
  }
  return document.body;
}

/**
 * 実クリック配列の再現 (FIX-VIEWERNAV-1と同一ヘルパ)。
 * pointerdown (bubbles) で祖先がsetPointerCaptureした場合は pointerup を
 * capture要素へリターゲットし、clickはdown/up対象の最近共通祖先で発火
 * させる — slotがcaptureするとcullボタンのonclickが発火しなくなる実挙動
 * と同一の意味論。素朴な .click() はpointer経由せず本bugを検出できない。
 */
function realClick(target: HTMLElement): { captured: boolean } {
  const patch = patchPointerCapture();
  try {
    target.dispatchEvent(
      new PointerEvent("pointerdown", {
        bubbles: true,
        cancelable: true,
        composed: true,
        pointerId: 3,
        isPrimary: true,
        button: 0,
        buttons: 1,
      }),
    );
    const upTarget = patch.calls[0]?.el ?? target;
    upTarget.dispatchEvent(
      new PointerEvent("pointerup", {
        bubbles: true,
        cancelable: true,
        composed: true,
        pointerId: 3,
        isPrimary: true,
        button: 0,
        buttons: 0,
      }),
    );
    nearestCommonAncestor(target, upTarget).dispatchEvent(
      new MouseEvent("click", { bubbles: true, cancelable: true, composed: true, button: 0 }),
    );
    return { captured: patch.calls.length > 0 };
  } finally {
    patch.restore();
  }
}

afterEach(() => {
  if (component) {
    unmount(component);
    component = null;
  }
  target?.remove();
  target = null;
});

describe("CompareView cullボタンとpointer capture (FIX-VIEWERNAV-2)", () => {
  it("cullボタンの実クリック配列 (pointerdown→pointerup→click) でcaptureせずonCullSlotが発火", async () => {
    const store = makeStore(3);
    await store.open(2, ["1"], 0);
    const cullCalls: Array<{ slot: number; value: "picked" | "rejected" }> = [];
    mountCompare(store, (slotIndex, value) => {
      cullCalls.push({ slot: slotIndex, value });
    });
    const reject = realClick(cullButton(0, "reject"));
    flushSync();
    expect(reject.captured).toBe(false);
    const pick = realClick(cullButton(1, "pick"));
    flushSync();
    expect(pick.captured).toBe(false);
    expect(cullCalls).toEqual([
      { slot: 0, value: "rejected" },
      { slot: 1, value: "picked" },
    ]);
    // ボタン押下はスロット操作 (active切替・パン) に参加しない
    expect(store.active).toBe(0);
  });

  it("cullボタン経由のpointerdownはドラッグを開始しない (skip後のmove/upは安全)", async () => {
    const store = makeStore(3);
    await store.open(2, ["1"], 0);
    store.setSlotViewport(0, 800, 600);
    mountCompare(store, () => {});
    const before = store.effectiveTransform(0).centerX;
    const slot = slotEl(0);
    const patch = patchPointerCapture();
    try {
      cullButton(0, "pick").dispatchEvent(
        new PointerEvent("pointerdown", {
          bubbles: true,
          cancelable: true,
          pointerId: 4,
          isPrimary: true,
          button: 0,
          buttons: 1,
          clientX: 60,
          clientY: 300,
        }),
      );
      slot.dispatchEvent(
        new PointerEvent("pointermove", {
          bubbles: true,
          cancelable: true,
          pointerId: 4,
          isPrimary: true,
          buttons: 1,
          clientX: 160,
          clientY: 300,
        }),
      );
      slot.dispatchEvent(
        new PointerEvent("pointerup", {
          bubbles: true,
          cancelable: true,
          pointerId: 4,
          isPrimary: true,
          button: 0,
          buttons: 0,
          clientX: 160,
          clientY: 300,
        }),
      );
    } finally {
      patch.restore();
    }
    expect(store.effectiveTransform(0).centerX).toBe(before);
    expect(patch.calls.length).toBe(0);
  });

  it("スロット面のpointerdownは従来どおりcaptureを開始しパンできる", async () => {
    const store = makeStore(3);
    await store.open(2, ["1"], 0);
    store.setSlotViewport(0, 800, 600);
    mountCompare(store, () => {});
    const before = store.effectiveTransform(0).centerX;
    const slot = slotEl(0);
    const patch = patchPointerCapture();
    try {
      slot.dispatchEvent(
        new PointerEvent("pointerdown", {
          bubbles: true,
          cancelable: true,
          pointerId: 5,
          isPrimary: true,
          button: 0,
          buttons: 1,
          clientX: 400,
          clientY: 300,
        }),
      );
      slot.dispatchEvent(
        new PointerEvent("pointermove", {
          bubbles: true,
          cancelable: true,
          pointerId: 5,
          isPrimary: true,
          buttons: 1,
          clientX: 450,
          clientY: 300,
        }),
      );
      expect(patch.calls.length).toBe(1);
      expect(patch.calls[0]!.el).toBe(slot);
      expect(patch.calls[0]!.pointerId).toBe(5);
    } finally {
      patch.restore();
    }
    expect(store.effectiveTransform(0).centerX).not.toBe(before);
    // pointerupでドラッグ終了 — 以降のmoveはパンしない
    slot.dispatchEvent(
      new PointerEvent("pointerup", {
        bubbles: true,
        cancelable: true,
        pointerId: 5,
        isPrimary: true,
        button: 0,
        buttons: 0,
        clientX: 450,
        clientY: 300,
      }),
    );
    const afterUp = store.effectiveTransform(0).centerX;
    slot.dispatchEvent(
      new PointerEvent("pointermove", {
        bubbles: true,
        cancelable: true,
        pointerId: 5,
        isPrimary: true,
        buttons: 1,
        clientX: 500,
        clientY: 300,
      }),
    );
    expect(store.effectiveTransform(0).centerX).toBe(afterUp);
  });
});
