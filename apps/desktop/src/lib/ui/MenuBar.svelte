<script lang="ts">
  /**
    * conventionalメニューバー (UIUX-1 / R-A)。
    *
    * - ファイル / 表示 / ツール / ヘルプ の項目定義はAppが渡す (MenuDef)。
    *   状態依存の有効無効はitemsのdisabledで毎評価される
    * - キーボード: トップレベルは矢印左右 (Home/End) で移動、
    *   Enter/Space/ArrowDownで開く。メニュー内は矢印上下で項目移動、
    *   矢印左右で隣メニューへ切替、Escapeで閉じてトリガへフォーカス復帰。
    *   Alt非依存 (OSのAltアクセラレータと衝突しない)
    * - クリック外・スクロールで閉じる。項目実行で閉じる
    * - 開いた直後に最初の有効項目へフォーカス (項目focus初期化)
    */
  import { onMount } from "svelte";
  import type { MenuAction } from "./ContextMenu.svelte";

  export type MenuDef = {
    id: string;
    label: string;
    items: MenuAction[];
  };

  let { menus }: { menus: MenuDef[] } = $props();

  let openIndex = $state<number | null>(null);
  /** トップレベルボタン (id → element)。フォーカス復帰と矢印移動に使う
   * (命令的参照のみで描画に反映しないためプレーンオブジェクト)。 */
  let topEls: Record<string, HTMLButtonElement | undefined> = {};
  let rootEl: HTMLElement | null = $state(null);

  /** each内のボタンをidへ登録 (フォーカス制御用。非reactiveな命令的参照)。 */
  function topAction(node: HTMLButtonElement, id: string): { destroy(): void } {
    topEls[id] = node;
    return {
      destroy() {
        delete topEls[id];
      },
    };
  }

  function focusTop(index: number): void {
    const menu = menus[index];
    if (menu === undefined) {
      return;
    }
    topEls[menu.id]?.focus();
  }

  function openMenu(index: number): void {
    const menu = menus[index];
    if (menu === undefined || menu.items.length === 0) {
      return;
    }
    openIndex = index;
    // popoverの描画後に先頭の有効項目へフォーカス (menuFocusActionで実行)
  }

  function closeMenu(restoreFocus: boolean): void {
    const index = openIndex;
    openIndex = null;
    if (restoreFocus && index !== null) {
      focusTop(index);
    }
  }

  async function runItem(item: MenuAction): Promise<void> {
    if (item.disabled) {
      return;
    }
    closeMenu(false);
    await item.run();
  }

  /** 開いたpopoverの最初の有効項目へフォーカス (Svelte action)。 */
  function menuFocusAction(node: HTMLElement): void {
    const first = Array.from(
      node.querySelectorAll<HTMLButtonElement>("button:not([disabled])"),
    )[0];
    first?.focus();
  }

  function enabledItems(): HTMLElement[] {
    const root = rootEl;
    if (root === null) {
      return [];
    }
    return Array.from(
      root.querySelectorAll<HTMLElement>(".menu-popover button:not([disabled])"),
    );
  }

  function handleKeydown(event: KeyboardEvent): void {
    if (event.ctrlKey || event.metaKey || event.altKey) {
      return;
    }
    const inMenu =
      openIndex !== null &&
      event.target instanceof Element &&
      event.target.closest(".menu-popover") !== null;
    if (inMenu) {
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        const items = enabledItems();
        if (items.length === 0) {
          return;
        }
        const current = items.indexOf(document.activeElement as HTMLElement);
        const delta = event.key === "ArrowDown" ? 1 : -1;
        items[(current + delta + items.length) % items.length]?.focus();
        return;
      }
      if (event.key === "ArrowRight" || event.key === "ArrowLeft") {
        event.preventDefault();
        const count = menus.length;
        openMenu(((openIndex ?? 0) + (event.key === "ArrowRight" ? 1 : -1) + count) % count);
        return;
      }
      if (event.key === "Escape") {
        event.preventDefault();
        closeMenu(true);
        return;
      }
      if (event.key === "Tab") {
        closeMenu(false);
        return;
      }
      return;
    }
    // トップレベル (menubar)
    switch (event.key) {
      case "ArrowRight":
      case "ArrowLeft": {
        event.preventDefault();
        const current = menus.findIndex(
          (menu) => topEls[menu.id] === document.activeElement,
        );
        const delta = event.key === "ArrowRight" ? 1 : -1;
        focusTop(((current === -1 ? 0 : current) + delta + menus.length) % menus.length);
        return;
      }
      case "Home":
        event.preventDefault();
        focusTop(0);
        return;
      case "End":
        event.preventDefault();
        focusTop(menus.length - 1);
        return;
      case "ArrowDown":
      case "Enter":
      case " ": {
        event.preventDefault();
        const current = menus.findIndex(
          (menu) => topEls[menu.id] === document.activeElement,
        );
        openMenu(current === -1 ? (openIndex ?? 0) : current);
        return;
      }      case "Escape":
        if (openIndex !== null) {
          event.preventDefault();
          closeMenu(true);
        }
        return;
      default:
        return;
    }
  }

  /** メニュー外のpointerdownで閉じる (フォーカス復帰なし — クリック先へ移る)。 */
  function handleWindowPointerDown(event: PointerEvent): void {
    if (
      openIndex !== null &&
      rootEl !== null &&
      event.target instanceof Node &&
      !rootEl.contains(event.target)
    ) {
      closeMenu(false);
    }
  }

  function handleTopPointerEnter(index: number): void {
    // 開いている間の別トップレベルhoverで切替 (conventional挙動)
    if (openIndex !== null && openIndex !== index) {
      openMenu(index);
    }
  }

  onMount(() => {
    window.addEventListener("pointerdown", handleWindowPointerDown, true);
    return () => {
      window.removeEventListener("pointerdown", handleWindowPointerDown, true);
    };
  });
</script>

<div
  bind:this={rootEl}
  class="menubar"
  role="menubar"
  aria-label="アプリケーションメニュー"
  tabindex="-1"
  onkeydown={handleKeydown}
>
  {#each menus as menu, index (menu.id)}
    <div class="menubar-top">
      <button
        type="button"
        class="menubar-button"
        role="menuitem"
        aria-haspopup="menu"
        aria-expanded={openIndex === index}
        use:topAction={menu.id}
        onclick={() => (openIndex === index ? closeMenu(true) : openMenu(index))}
        onpointerenter={() => handleTopPointerEnter(index)}
      >
        {menu.label}
      </button>
      {#if openIndex === index}
        <div class="menu-popover" role="menu" aria-label={menu.label} use:menuFocusAction>
          {#each menu.items as item (item.id)}
            <button
              type="button"
              role="menuitemcheckbox"
              aria-checked={item.checked === undefined ? undefined : item.checked}
              disabled={item.disabled}
              title={item.title ?? ""}
              data-testid={item.testId}
              onclick={() => void runItem(item)}
            >{#if item.checked === true}<span class="menu-check" aria-hidden="true">✓</span>{/if}{item.label}</button>
          {/each}
        </div>
      {/if}
    </div>
  {/each}
</div>

<style>
  .menubar {
    display: flex;
    align-items: stretch;
    gap: 0.1rem;
    padding: 0 0.4rem;
    font-size: 0.82rem;
    border-bottom: 1px solid light-dark(#d0d7de, #30363d);
  }

  .menubar-top {
    position: relative;
  }

  .menubar-button {
    border: none;
    background: transparent;
    color: inherit;
    font: inherit;
    padding: 0.22rem 0.6rem;
    border-radius: 0.375rem 0.375rem 0 0;
    cursor: pointer;
  }

  .menubar-button:hover,
  .menubar-button:focus-visible,
  .menubar-button[aria-expanded="true"] {
    background: light-dark(#eaeef2, #21262d);
  }

  .menu-popover {
    position: absolute;
    left: 0;
    top: 100%;
    z-index: 500;
    min-width: 15rem;
    max-height: 70vh;
    overflow-y: auto;
    padding: 0.25rem;
    background: light-dark(#ffffff, #1c2128);
    border: 1px solid light-dark(#d0d7de, #444c56);
    border-radius: 0.5rem;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.25);
  }

  .menu-popover button {
    display: block;
    width: 100%;
    padding: 0.35rem 0.6rem;
    border: none;
    border-radius: 0.375rem;
    background: transparent;
    color: inherit;
    text-align: left;
    font: inherit;
    cursor: pointer;
  }

  .menu-popover button:hover:not(:disabled),
  .menu-popover button:focus-visible {
    background: light-dark(#0969da, #1f6feb);
    color: #ffffff;
  }

  .menu-popover button:disabled {
    opacity: 0.45;
    cursor: default;
  }

  .menu-check {
    display: inline-block;
    width: 1em;
    margin-left: -0.4em;
    padding-right: 0.1em;
  }
</style>
