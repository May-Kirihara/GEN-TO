// @vitest-environment happy-dom
/**
 * LeftPaneTabs.svelte コンポーネントテスト (UIUX-1 / R-B)。
 *
 * - 3タブ (フォルダ/コレクション/ライブラリ) のrole=tablist描画
 * - クリック・矢印左右/Home/Endで切替通知 (自動活性化)
 * - aria-selected / aria-controls / ロービングtabindex
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import LeftPaneTabs from "./LeftPaneTabs.svelte";

let target: HTMLElement | null = null;
let component: Record<string, unknown> | null = null;
let selected: string[] = [];

function mountTabs(active: "folders" | "collections" | "library" = "folders"): void {
  target = document.createElement("div");
  document.body.appendChild(target);
  component = mount(LeftPaneTabs, {
    target,
    props: { active, onChange: (tab) => selected.push(tab) },
  });
  flushSync();
}

function tabs(): HTMLButtonElement[] {
  return Array.from(target!.querySelectorAll<HTMLButtonElement>("[role='tab']"));
}

function key(el: HTMLElement, key: string): void {
  el.dispatchEvent(
    new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true }),
  );
  flushSync();
}

beforeEach(() => {
  selected = [];
});

afterEach(() => {
  if (component) {
    unmount(component);
    component = null;
  }
  target?.remove();
  target = null;
});

describe("LeftPaneTabs", () => {
  it("3タブを描画し、アクティブタブのaria-selectedとtabindexが正しい", () => {
    mountTabs("collections");
    const labels = tabs().map((b) => b.textContent);
    expect(labels).toEqual(["フォルダ", "コレクション", "ライブラリ"]);
    const activeTab = tabs()[1]!;
    expect(activeTab.getAttribute("aria-selected")).toBe("true");
    expect(activeTab.tabIndex).toBe(0);
    expect(tabs()[0]!.getAttribute("aria-selected")).toBe("false");
    expect(tabs()[0]!.tabIndex).toBe(-1);
    expect(activeTab.getAttribute("aria-controls")).toBe("left-panel-collections");
    expect(activeTab.id).toBe("left-tab-collections");
  });

  it("クリックで切替を通知する (アクティブタブ自体のクリックは通知しない)", () => {
    mountTabs("folders");
    tabs()[0]!.click();
    flushSync();
    expect(selected).toEqual([]);
    tabs()[2]!.click();
    flushSync();
    expect(selected).toEqual(["library"]);
  });

  it("矢印左右で隣タブへ移動 (循環) しHome/Endも機能する", () => {
    mountTabs("folders");
    key(target!.querySelector("[role='tablist']") as HTMLElement, "ArrowRight");
    expect(selected).toEqual(["collections"]);
    // activeは親が更新するため、段階ごとに活性タブを変えて検証
    mountTabs("collections");
    key(target!.querySelector("[role='tablist']") as HTMLElement, "ArrowLeft");
    expect(selected.at(-1)).toBe("folders");
    mountTabs("collections");
    key(target!.querySelector("[role='tablist']") as HTMLElement, "ArrowRight");
    expect(selected.at(-1)).toBe("library");
    mountTabs("folders");
    key(target!.querySelector("[role='tablist']") as HTMLElement, "ArrowLeft");
    expect(selected.at(-1)).toBe("library");
    mountTabs("folders");
    key(target!.querySelector("[role='tablist']") as HTMLElement, "End");
    expect(selected.at(-1)).toBe("library");
    mountTabs("library");
    key(target!.querySelector("[role='tablist']") as HTMLElement, "Home");
    expect(selected.at(-1)).toBe("folders");
  });
});
