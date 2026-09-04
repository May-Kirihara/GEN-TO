// @vitest-environment happy-dom
/**
 * MenuBar.svelte コンポーネントテスト (UIUX-1 / R-A)。
 *
 * - トップレベル4メニューの描画とクリック開閉
 * - 開いた直後に最初の**有効**項目へフォーカス (項目focus初期化)
 * - キーボード: Enter/ArrowDownで開く・矢印上下で項目移動・ArrowRightで隣メニュー・
 *   Escapeで閉じてトリガへフォーカス復帰
 * - 無効項目は実行されない。項目実行で閉じる
 * - メニュー外pointerdownで閉じる
 * - itemsのdisabled変更が開いたメニューへ即時反映される (動的分岐)
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import MenuBar, { type MenuDef } from "./MenuBar.svelte";

let target: HTMLElement | null = null;
let component: Record<string, unknown> | null = null;

function menusOf(overrides: { exportDisabled?: boolean } = {}): MenuDef[] {
  return [
    {
      id: "file",
      label: "ファイル",
      items: [
        { id: "scan", label: "一時走査を実行", run: vi.fn() },
        {
          id: "export",
          label: "SD学習用エクスポート…",
          run: vi.fn(),
          disabled: overrides.exportDisabled,
        },
        { id: "quit", label: "終了", run: vi.fn() },
      ],
    },
    {
      id: "view",
      label: "表示",
      items: [
        { id: "left", label: "左ペインを表示", run: vi.fn(), checked: true },
        { id: "right", label: "右ペインを表示", run: vi.fn(), checked: false },
      ],
    },
    {
      id: "tools",
      label: "ツール",
      items: [{ id: "settings", label: "設定…", run: vi.fn(), testId: "settings-button" }],
    },
    {
      id: "help",
      label: "ヘルプ",
      items: [{ id: "about", label: "このアプリについて", run: vi.fn() }],
    },
  ];
}

function mountBar(menus: MenuDef[]): void {
  target = document.createElement("div");
  document.body.appendChild(target);
  component = mount(MenuBar, { target, props: { menus } });
  flushSync();
}

function topButtons(): HTMLButtonElement[] {
  return Array.from(target!.querySelectorAll<HTMLButtonElement>(".menubar-button"));
}

function popoverItems(): HTMLButtonElement[] {
  return Array.from(target!.querySelectorAll<HTMLButtonElement>(".menu-popover button"));
}

function key(target: HTMLElement, key: string, shift = false): void {
  target.dispatchEvent(
    new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true, shiftKey: shift }),
  );
  flushSync();
}

beforeEach(() => {
  vi.clearAllMocks();
});

afterEach(() => {
  if (component) {
    unmount(component);
    component = null;
  }
  target?.remove();
  target = null;
});

describe("MenuBar", () => {
  it("トップレベル4メニューをrole=menubarで描画する", () => {
    mountBar(menusOf());
    const bar = target!.querySelector("[role='menubar']");
    expect(bar).not.toBeNull();
    expect(topButtons().map((b) => b.textContent)).toEqual([
      "ファイル",
      "表示",
      "ツール",
      "ヘルプ",
    ]);
    // 開いていなければpopoverはない
    expect(target!.querySelector(".menu-popover")).toBeNull();
  });

  it("クリックで開き、最初の有効項目へフォーカス、再クリックで閉じる", () => {
    mountBar(menusOf());
    topButtons()[0]!.click();
    flushSync();
    const items = popoverItems();
    expect(items.map((b) => b.textContent?.trim())).toEqual([
      "一時走査を実行",
      "SD学習用エクスポート…",
      "終了",
    ]);
    expect(items[0]).toBe(document.activeElement);
    expect(topButtons()[0]!.getAttribute("aria-expanded")).toBe("true");
    topButtons()[0]!.click();
    flushSync();
    expect(target!.querySelector(".menu-popover")).toBeNull();
  });

  it("項目クリックでactionが走りメニューが閉じる (testidも付与)", () => {
    const menus = menusOf();
    mountBar(menus);
    topButtons()[2]!.click();
    flushSync();
    const settings = target!.querySelector<HTMLButtonElement>('[data-testid="settings-button"]');
    expect(settings).not.toBeNull();
    settings!.click();
    flushSync();
    expect(menus[2]!.items[0]!.run).toHaveBeenCalledTimes(1);
    expect(target!.querySelector(".menu-popover")).toBeNull();
  });

  it("無効項目はクリックしても実行されない", () => {
    const menus = menusOf({ exportDisabled: true });
    mountBar(menus);
    topButtons()[0]!.click();
    flushSync();
    const exportItem = popoverItems()[1]!;
    expect(exportItem.disabled).toBe(true);
    exportItem.click();
    flushSync();
    expect(menus[0]!.items[1]!.run).not.toHaveBeenCalled();
    // メニューは閉じない (runItemはdisabledでreturn)
    expect(target!.querySelector(".menu-popover")).not.toBeNull();
  });

  it("チェック付き項目 (aria-checked) を描画する", () => {
    mountBar(menusOf());
    topButtons()[1]!.click();
    flushSync();
    const items = popoverItems();
    expect(items[0]!.getAttribute("aria-checked")).toBe("true");
    expect(items[0]!.textContent).toContain("✓");
    expect(items[1]!.getAttribute("aria-checked")).toBe("false");
    expect(items[1]!.textContent).not.toContain("✓");
  });

  it("通常コマンドはrole=menuitem・チェック項目のみmenuitemcheckbox (r1 N1)", () => {
    mountBar(menusOf());
    topButtons()[0]!.click(); // ファイル: 走査/エクスポート/終了 = すべて通常コマンド
    flushSync();
    for (const item of popoverItems()) {
      expect(item.getAttribute("role")).toBe("menuitem");
      expect(item.hasAttribute("aria-checked")).toBe(false);
    }
    topButtons()[0]!.click();
    flushSync();
    topButtons()[1]!.click(); // 表示: 左ペイン/右ペイン = チェック付き
    flushSync();
    for (const item of popoverItems()) {
      expect(item.getAttribute("role")).toBe("menuitemcheckbox");
      expect(item.hasAttribute("aria-checked")).toBe(true);
    }
  });

  it("キーボード: トップレベルの矢印左右で移動し、Enterで開いて項目へフォーカス", () => {
    mountBar(menusOf());
    const [file, view] = topButtons();
    file!.focus();
    key(file!, "ArrowRight");
    expect(document.activeElement).toBe(view);
    key(view!, "Home");
    expect(document.activeElement).toBe(file);
    key(file!, "End");
    expect(document.activeElement).toBe(topButtons()[3]);
    topButtons()[0]!.focus();
    key(topButtons()[0]!, "Enter");
    expect(target!.querySelector(".menu-popover")).not.toBeNull();
    expect(popoverItems()[0]).toBe(document.activeElement);
  });

  it("キーボード: メニュー内の矢印上下で循環移動し、Escapeでトリガへ復帰", () => {
    mountBar(menusOf());
    const file = topButtons()[0]!;
    file.click();
    flushSync();
    const items = popoverItems();
    expect(items[0]).toBe(document.activeElement);
    key(items[0]!, "ArrowDown");
    expect(items[1]).toBe(document.activeElement);
    key(items[1]!, "ArrowDown");
    key(document.activeElement as HTMLElement, "ArrowDown");
    expect(items[0]).toBe(document.activeElement); // 循環
    key(document.activeElement as HTMLElement, "ArrowUp");
    expect(items[2]).toBe(document.activeElement);
    key(document.activeElement as HTMLElement, "Escape");
    expect(target!.querySelector(".menu-popover")).toBeNull();
    expect(document.activeElement).toBe(file);
  });

  it("キーボード: メニュー開放中のArrowRightで隣メニューへ切替", () => {
    mountBar(menusOf());
    topButtons()[0]!.click();
    flushSync();
    const items = popoverItems();
    key(items[0]!, "ArrowRight");
    // 表示メニューへ切替 (項目は表示の2項目。空白正規化で比較)
    const nextItems = popoverItems();
    expect(nextItems.map((b) => (b.textContent ?? "").replace(/\s+/g, ""))).toEqual([
      "✓左ペインを表示",
      "右ペインを表示",
    ]);
    expect(nextItems[0]).toBe(document.activeElement);
  });

  it("メニュー外のpointerdownで閉じる", () => {
    mountBar(menusOf());
    topButtons()[0]!.click();
    flushSync();
    expect(target!.querySelector(".menu-popover")).not.toBeNull();
    const outside = document.createElement("button");
    document.body.appendChild(outside);
    outside.dispatchEvent(
      new PointerEvent("pointerdown", { bubbles: true, cancelable: true }),
    );
    flushSync();
    expect(target!.querySelector(".menu-popover")).toBeNull();
    outside.remove();
  });

  it("itemsのdisabled変更が開いているメニューへ即時反映される", () => {
    const menus = menusOf({ exportDisabled: false });
    mountBar(menus);
    topButtons()[0]!.click();
    flushSync();
    expect(popoverItems()[1]!.disabled).toBe(false);
    // props差し替え (スキャン状態変化に相当): 再mountで検証
    unmount(component!);
    component = null;
    const updated = menusOf({ exportDisabled: true });
    mountBar(updated);
    topButtons()[0]!.click();
    flushSync();
    expect(popoverItems()[1]!.disabled).toBe(true);
  });
});
