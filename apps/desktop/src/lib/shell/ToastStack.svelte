<script lang="ts">
  /**
    * 揮発性noticeのトースト表示 (UIUX-5/WP4。WP7aでApp.svelteから抽出)。
    * toastsの寿命管理 (追加・4秒自動消滅timer) はAppが正本 —
    * ここは受取った配列の表示と×ボタンによる手動消去のみ。
    */
  type ToastEntry = { id: number; kind: "viewer" | "fileops" | "meta" | "os"; text: string };

  let {
    toasts,
    onDismissToast,
  }: {
    toasts: ToastEntry[];
    /** ×ボタンでの手動消去 (AppのdismissToast。自動消滅はApp側timer)。 */
    onDismissToast: (id: number) => void;
  } = $props();
</script>

<div class="toast-stack" aria-live="polite">
  {#each toasts as toast (toast.id)}
    <div class="toast">
      {#if toast.kind === "fileops"}
        <p class="toast-message" role="status" data-testid="fileops-notice">{toast.text}</p>
      {:else}
        <p class="toast-message">{toast.text}</p>
      {/if}
      <button
        class="toast-close"
        aria-label="通知を閉じる"
        title="通知を閉じる"
        onclick={() => onDismissToast(toast.id)}
      >×</button>
    </div>
  {/each}
</div>

<style>
  /* UIUX-5/WP4: 揮発性noticeのトースト (画面右下固定・4秒自動消滅)。 */
  .toast-stack {
    position: fixed;
    right: var(--space-60);
    bottom: var(--space-60);
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: var(--space-40);
    z-index: 600;
    /* スタック領域はポインタを貫通し、トースト本体のみ操作可能 */
    pointer-events: none;
  }

  .toast {
    pointer-events: auto;
    display: flex;
    align-items: center;
    gap: var(--space-40);
    max-width: 28rem;
    padding: var(--space-40) var(--space-60);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-sm);
    background: var(--color-surface-raised);
    box-shadow: var(--shadow-popover);
    font-size: var(--font-size-80);
  }

  .toast-message {
    margin: 0;
    overflow-wrap: anywhere;
  }

  .toast-close {
    flex: none;
    border: none;
    background: transparent;
    color: inherit;
    font: inherit;
    padding: 0;
    line-height: 1;
    cursor: pointer;
  }

  /* App.svelteの全button共通則と同一値 (scoped CSSのためcomponent毎に必要)。 */
  button {
    font-size: var(--font-size-80);
  }
</style>
