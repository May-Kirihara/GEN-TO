// @vitest-environment happy-dom
/**
 * CollectionsPane.svelte コンポーネントテスト (UIUX-2 C1)。
 *
 * 改名入力が既存テキストを選択済みで開くこと (ThumbnailGridの
 * リネームと同一挙動) を実際の描画で検証する。
 */
import { afterEach, describe, expect, it } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import CollectionsPane from "./CollectionsPane.svelte";
import type { CollectionDto, CollectionsStore } from "../collections";

function collection(name: string): CollectionDto {
  return {
    id: "1",
    uuid: "00000000-0000-7000-8000-000000000001",
    name,
    kind: "manual",
    item_count: 3,
    created_at: "2026-09-04T00:00:00.000Z",
    updated_at: "2026-09-04T00:00:00.000Z",
  };
}

function makeStore(collections: CollectionDto[]): CollectionsStore {
  return {
    collections,
    onChange: () => () => {},
    createManual: async () => {},
    createSmart: async () => {},
    rename: async () => {},
    remove: async () => {},
    refreshSmart: async () => {},
    items: async () => [],
    show: async () => {},
  } as unknown as CollectionsStore;
}

let target: HTMLElement | null = null;
const components: Array<ReturnType<typeof mount>> = [];

afterEach(() => {
  while (components.length > 0) {
    unmount(components.pop()!);
  }
  target?.remove();
  target = null;
});

describe("CollectionsPane 改名入力 (UIUX-2 C1)", () => {
  it("改名ボタンで既存テキストが選択済みの入力になる (全置換しやすい)", () => {
    target = document.createElement("div");
    document.body.appendChild(target);
    components.push(
      mount(CollectionsPane, {
        target,
        props: {
          store: makeStore([collection("旅行2026")]),
          activeCollectionId: null,
          onSelect: () => {},
        },
      }),
    );
    flushSync();

    const renameButton = Array.from(
      document.querySelectorAll<HTMLButtonElement>(".coll-icon-button"),
    ).find((b) => b.title === "改名");
    expect(renameButton).toBeDefined();
    renameButton?.click();
    flushSync();

    const input = document.querySelector<HTMLInputElement>(".coll-name-input.rename");
    expect(input).not.toBeNull();
    expect(document.activeElement).toBe(input);
    expect(input?.value).toBe("旅行2026");
    expect(input?.selectionStart).toBe(0);
    expect(input?.selectionEnd).toBe("旅行2026".length);
  });
});
