// @vitest-environment happy-dom
/**
 * ImageViewer.svelte コンポーネントテスト (UIUX-3 V-4)。
 *
 * - 左右端の送りボタン: aria-label・端でdisabled・total≤1・クリックでnav
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
