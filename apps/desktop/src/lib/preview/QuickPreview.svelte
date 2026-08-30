<script lang="ts">
  /**
   * クイックプレビューオーバーレイ (MVP WP-4 / 仕様§8)。
   *
   * - 現在セルを中央拡大表示 (preview2048 / アニメーションはoriginal)
   * - Space/Escで閉じる、←→で移動 (キー処理自体はAppのKeyDispatcher)
   * - 下部にファイル名・寸法・位置と操作ヒントを表示
   */
  import { onDestroy } from "svelte";
  import type { QuickPreviewStore } from "./quick-preview";

  let {
    store,
    onClose,
  }: {
    store: QuickPreviewStore;
    onClose: () => void;
  } = $props();

  // GridStore等と同様、版番号で変更通知を受ける
  let tick = $state(0);
  // svelte-ignore state_referenced_locally
  const stopNotify = store.onChange(() => {
    tick++;
  });

  function currentUrl(): string | null {
    void tick;
    return store.url;
  }

  function currentItem() {
    void tick;
    return store.item;
  }

  function noteText(): string {
    void tick;
    if (store.item !== null && store.item.state !== "ready") {
      return `表示できません (状態: ${store.item.state})`;
    }
    if (store.loading) {
      return "読み込み中…";
    }
    return store.lastError ?? "表示する画像がありません";
  }

  function noteIsError(): boolean {
    void tick;
    return !store.loading && (store.item === null || store.item.state !== "ready" || store.url === null);
  }

  function formatDims(): string {
    void tick;
    const item = store.item;
    if (item === null || item.width === null || item.height === null) {
      return "—";
    }
    return `${item.width}×${item.height}`;
  }

  function positionText(): string {
    void tick;
    return `${store.index + 1}/${store.total()}`;
  }

  onDestroy(() => {
    stopNotify();
  });
  function isActive(): boolean {
    void tick;
    return store.active;
  }
</script>

{#if isActive()}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div
    class="quick-preview-backdrop"
    onclick={onClose}
    role="presentation"
  >
    <div
      class="quick-preview-stage"
      role="dialog"
      aria-label="クイックプレビュー"
      tabindex="-1"
      onclick={(event) => event.stopPropagation()}
    >
      {#if currentUrl() !== null}
        <img src={currentUrl() ?? ""} alt={currentItem()?.displayName ?? ""} />
      {:else if currentItem() !== null && currentItem()?.state !== "ready"}
        <div class="qp-note error">
          {noteText()}
        </div>
      {:else}
        <div class="qp-note" class:error={noteIsError()}>{noteText()}</div>
      {/if}
    </div>
    <div class="quick-preview-caption">
      <span class="qp-name">{currentItem()?.displayName ?? "—"}</span>
      <span class="qp-meta">{formatDims()} · {positionText()}</span>
    </div>
    <div class="quick-preview-hint">
      Space/Esc: 閉じる · ←→: 前後へ · Enter: ビューア
    </div>
  </div>
{/if}

<style>
  .quick-preview-backdrop {
    position: fixed;
    inset: 0;
    z-index: 500;
    background: rgba(0, 0, 0, 0.72);
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 0.75rem;
  }

  .quick-preview-stage {
    max-width: 86vw;
    max-height: 78vh;
    display: flex;
    align-items: center;
    justify-content: center;
    background:
      repeating-conic-gradient(#808080 0% 25%, transparent 0% 50%) 0 0 / 24px 24px;
    border-radius: 0.5rem;
    overflow: hidden;
    box-shadow: 0 12px 48px rgba(0, 0, 0, 0.5);
  }

  .quick-preview-stage img {
    display: block;
    max-width: 86vw;
    max-height: 78vh;
    object-fit: contain;
  }

  .quick-preview-caption {
    display: flex;
    gap: 1rem;
    color: #f0f3f6;
    font-size: 0.95rem;
    background: rgba(0, 0, 0, 0.45);
    padding: 0.35rem 0.75rem;
    border-radius: 0.5rem;
  }

  .qp-meta {
    color: #9aa4b2;
  }

  .quick-preview-hint {
    color: #9aa4b2;
    font-size: 0.8rem;
  }

  .qp-note {
    color: #d0d7de;
    padding: 3rem 4rem;
  }

  .qp-note.error {
    color: #f85149;
  }
</style>
