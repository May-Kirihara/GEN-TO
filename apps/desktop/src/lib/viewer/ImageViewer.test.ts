// @vitest-environment happy-dom
/**
 * ImageViewer.svelte コンポーネントテスト (UIUX-3 V-4)。
 *
 * - 左右端の送りボタン: aria-label・端でdisabled・total≤1・クリックでnav
 * - 実クリック配列 (pointerdown→pointerup→click): navボタンはstageの
 *   pointer capture対象外 (FIX-VIEWERNAV-1)・画像面は従来どおりcapture開始
 * - 平常wheel=画像送り (accumulator: 1ジェスチャ=1送り)・常にpreventDefault
 * - Alt/Ctrl+wheel=ズーム (pinch=ctrl保存)
 * を実ViewerStore + 偽IPC/itemsで検証する。
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import ImageViewer from "./ImageViewer.svelte";
import { ViewerStore, type ViewerIpc } from "./viewer-store";
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

function makeIpc(): ViewerIpc {
  return {
    invoke: async <T>(): Promise<T> => ({ queued: 0 }) as T,
    listen: async () => () => {},
  };
}

function makeStore(totalCount: number, now: () => number = () => 0): ViewerStore {
  const items = Array.from({ length: totalCount }, (_, i) => item(i));
  const store = new ViewerStore(
    makeIpc(),
    {
      total: () => items.length,
      get: (index) => items[index] ?? null,
      ensureRange: async () => {},
    },
    now,
  );
  store.setViewport(800, 600);
  return store;
}

let target: HTMLElement | null = null;
let component: Record<string, unknown> | null = null;

function mountViewer(store: ViewerStore): void {
  target = document.createElement("div");
  document.body.appendChild(target);
  component = mount(ImageViewer, {
    target,
    props: { store, onClose: () => {}, triageMode: false },
  });
  flushSync();
}

function navButton(dir: "prev" | "next"): HTMLButtonElement {
  const el = target!.querySelector<HTMLButtonElement>(
    `[data-testid='viewer-nav-${dir}']`,
  );
  expect(el, `viewer-nav-${dir}`).not.toBeNull();
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
 * 実クリック配列の再現 (FIX-VIEWERNAV-1)。
 * pointerdown (bubbles) で祖先がsetPointerCaptureした場合は pointerup を
 * capture要素へリターゲットし、clickはdown/up対象の最近共通祖先で発火
 * させる — stageがcaptureするとbuttonのonclickが発火しなくなる実挙動と
 * 同一の意味論。素朴な .click() はpointer経由せず本bugを検出できない。
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

function wheel(deltaY: number, mods: { altKey?: boolean; ctrlKey?: boolean } = {}): WheelEvent {
  const event = new WheelEvent("wheel", {
    bubbles: true,
    cancelable: true,
    clientX: 100,
    clientY: 100,
    deltaY,
  });
  // happy-dom 20のWheelEventは修飾キーinitを黙って落とすため補完
  // (wheel-speed.test.tsの既知制約と同じ対策)
  for (const [key, value] of Object.entries(mods)) {
    Object.defineProperty(event, key, { get: () => value });
  }
  target!.querySelector<HTMLElement>(".viewer-stage")!.dispatchEvent(event);
  return event;
}

beforeEach(() => {
  vi.restoreAllMocks();
});

afterEach(() => {
  if (component) {
    unmount(component);
    component = null;
  }
  target?.remove();
  target = null;
});

describe("ImageViewer 送りボタン (UIUX-3 V-4)", () => {
  it("左右端にaria-label付きボタン・端 (index=0) では前がdisabled・次が有効", () => {
    const store = makeStore(3);
    mountViewer(store);
    const prev = navButton("prev");
    const next = navButton("next");
    expect(prev.getAttribute("aria-label")).toBe("前の画像");
    expect(next.getAttribute("aria-label")).toBe("次の画像");
    expect(prev.disabled).toBe(true);
    expect(next.disabled).toBe(false);
  });

  it("次ボタンでnav(+1)・前ボタンでnav(-1)・末尾では次がdisabled", () => {
    const store = makeStore(3);
    mountViewer(store);
    navButton("next").click();
    flushSync();
    expect(store.displayIndex).toBe(1);
    expect(navButton("prev").disabled).toBe(false);
    expect(navButton("next").disabled).toBe(false);
    navButton("next").click();
    flushSync();
    expect(store.displayIndex).toBe(2);
    expect(navButton("next").disabled).toBe(true);
    navButton("prev").click();
    flushSync();
    expect(store.displayIndex).toBe(1);
  });

  it("total≤1では両ボタンdisabled", () => {
    const store = makeStore(1);
    mountViewer(store);
    expect(navButton("prev").disabled).toBe(true);
    expect(navButton("next").disabled).toBe(true);
  });
});

describe("ImageViewer 実クリック配列とpointer capture (FIX-VIEWERNAV-1)", () => {
  it("navボタンの実クリック配列 (pointerdown→pointerup→click) でcaptureせずnavが発火", () => {
    const store = makeStore(3);
    mountViewer(store);
    const next = realClick(navButton("next"));
    flushSync();
    expect(next.captured).toBe(false);
    expect(store.displayIndex).toBe(1);
    const prev = realClick(navButton("prev"));
    flushSync();
    expect(prev.captured).toBe(false);
    expect(store.displayIndex).toBe(0);
  });

  it("navボタン経由のpointerdownはドラッグを開始しない (skip後のmove/upは安全)", async () => {
    const store = makeStore(3);
    await store.open(0);
    store.setMode("actual");
    mountViewer(store);
    const before = store.transform.centerX;
    const stage = target!.querySelector<HTMLElement>(".viewer-stage")!;
    const patch = patchPointerCapture();
    try {
      navButton("next").dispatchEvent(
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
      stage.dispatchEvent(
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
      stage.dispatchEvent(
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
    expect(store.transform.centerX).toBe(before);
    expect(patch.calls.length).toBe(0);
  });

  it("画像面 (stage) のpointerdownは従来どおりcaptureを開始しパンできる", async () => {
    const store = makeStore(3);
    await store.open(0);
    store.setMode("actual");
    mountViewer(store);
    const before = store.transform.centerX;
    const stage = target!.querySelector<HTMLElement>(".viewer-stage")!;
    const patch = patchPointerCapture();
    try {
      stage.dispatchEvent(
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
      stage.dispatchEvent(
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
    } finally {
      // capture開始の表明はdrag中のパッチ有効期間内で行う
      expect(patch.calls.length).toBe(1);
      expect(patch.calls[0]!.el).toBe(stage);
      expect(patch.calls[0]!.pointerId).toBe(5);
      patch.restore();
    }
    expect(store.transform.centerX).not.toBe(before);
    // pointerupでドラッグ終了 — 以降のmoveはパンしない
    stage.dispatchEvent(
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
    const afterUp = store.transform.centerX;
    stage.dispatchEvent(
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
    expect(store.transform.centerX).toBe(afterUp);
  });
});

describe("ImageViewer wheel再定義 (UIUX-3 V-4)", () => {
  it("平常wheel (上=deltaY<0) は前の画像へ送る・preventDefaultされる", () => {
    const store = makeStore(3);
    store.nav(2);
    mountViewer(store);
    const event = wheel(-100);
    expect(store.displayIndex).toBe(1);
    expect(event.defaultPrevented).toBe(true);
  });

  it("平常wheel (下=deltaY>0) は次の画像へ送る", () => {
    const store = makeStore(3);
    mountViewer(store);
    wheel(100);
    expect(store.displayIndex).toBe(1);
  });

  it("1ジェスチャ=1送り: 同時刻に3ノッチdispatchしても1枚しか進まない (accumulator)", () => {
    const store = makeStore(10);
    mountViewer(store);
    wheel(100);
    wheel(100);
    wheel(100);
    expect(store.displayIndex).toBe(1);
  });

  it("Alt+wheelはズーム (表示index不変・transform変化)", async () => {
    const store = makeStore(3);
    await store.open(0);
    mountViewer(store);
    const before = store.transform.scale;
    const beforeMode = store.transform.mode;
    wheel(-100, { altKey: true });
    expect(store.displayIndex).toBe(0);
    expect(store.transform.mode !== beforeMode || store.transform.scale !== before).toBe(true);
  });

  it("Ctrl+wheel (pinch) はズームとして保存される", async () => {
    const store = makeStore(3);
    await store.open(0);
    mountViewer(store);
    const before = store.transform.scale;
    wheel(-100, { ctrlKey: true });
    expect(store.displayIndex).toBe(0);
    expect(store.transform.scale).not.toBe(before);
  });
});
