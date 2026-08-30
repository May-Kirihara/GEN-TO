<script lang="ts">
  /**
   * 汎用コンテキストメニュー (MVP WP-4)。
   *
   * - 指定座標 (クリック位置) に出し、項目クリックでactionを実行して閉じる
   * - 外部クリック・Esc・スクロールで閉じる
   * - メニュー項目はApp側で構築 (OS連携・クリップボードなど)
   */
  import { onMount } from "svelte";

  export type MenuAction = {
    id: string;
    label: string;
    run: () => void | Promise<void>;
    disabled?: boolean;
  };

  let {
    x,
    y,
    actions,
    onClose,
  }: {
    x: number;
    y: number;
    actions: MenuAction[];
    onClose: () => void;
  } = $props();

  let menuEl: HTMLElement | null = $state(null);
  /** 画面端で溢れる場合の補正後座標 (effect確定までは元の座標を使う)。 */
  let placed = $state<{ x: number; y: number } | null>(null);

  $effect(() => {
    // 描画後に実寸から配置を確定する (初回はx,yをそのまま使う)
    const el = menuEl;
    if (el === null) {
      return;
    }
    const rect = el.getBoundingClientRect();
    const maxX = window.innerWidth - rect.width - 8;
    const maxY = window.innerHeight - rect.height - 8;
    placed = { x: Math.max(8, Math.min(x, maxX)), y: Math.max(8, Math.min(y, maxY)) };
  });

  function handleKeydown(event: KeyboardEvent): void {
    if (event.key === "Escape") {
      event.preventDefault();
      onClose();
    }
  }

  /** メニュー外のpointerdownで閉じる。委托eventでも要素判定は成立する。 */
  function handleWindowPointerDown(event: PointerEvent): void {
    if (menuEl !== null && event.target instanceof Node && menuEl.contains(event.target)) {
      return;
    }
    onClose();
  }

  function handleWindowWheel(): void {
    onClose();
  }

  onMount(() => {
    window.addEventListener("keydown", handleKeydown, true);
    window.addEventListener("pointerdown", handleWindowPointerDown, true);
    window.addEventListener("wheel", handleWindowWheel, true);
    return () => {
      window.removeEventListener("keydown", handleKeydown, true);
      window.removeEventListener("pointerdown", handleWindowPointerDown, true);
      window.removeEventListener("wheel", handleWindowWheel, true);
    };
  });

  async function runAction(action: MenuAction): Promise<void> {
    if (action.disabled) {
      return;
    }
    onClose();
    await action.run();
  }
</script>

<div
  bind:this={menuEl}
  class="context-menu"
  style="left:{placed?.x ?? x}px; top:{placed?.y ?? y}px"
  role="menu"
  tabindex="-1"
>
  {#each actions as action (action.id)}
    <button
      role="menuitem"
      disabled={action.disabled}
      onclick={() => void runAction(action)}
    >
      {action.label}
    </button>
  {/each}
</div>

<style>
  .context-menu {
    position: fixed;
    z-index: 600;
    min-width: 14rem;
    padding: 0.25rem;
    background: light-dark(#ffffff, #1c2128);
    border: 1px solid light-dark(#d0d7de, #444c56);
    border-radius: 0.5rem;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.25);
  }

  .context-menu button {
    display: block;
    width: 100%;
    padding: 0.4rem 0.6rem;
    border: none;
    border-radius: 0.375rem;
    background: transparent;
    color: inherit;
    text-align: left;
    font: inherit;
    cursor: pointer;
  }

  .context-menu button:hover:not(:disabled) {
    background: light-dark(#0969da, #1f6feb);
    color: #ffffff;
  }

  .context-menu button:disabled {
    opacity: 0.45;
    cursor: default;
  }
</style>
