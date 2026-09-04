<script lang="ts">
  /**
    * 「コレクションへ追加」ピッカー (MVP WP-6)。
    *
    * グリッドのコンテキストメニュー「コレクションへ追加…」から開く。
    * 既存manualコレクションへの一括追加と、新規コレクションへの追加
    * (名前入力) を担う。smartコレクションは条件駆動のため追加対象外
    * (表示で説明)。
    */
  import { onDestroy } from "svelte";
  import type { CollectionDto, CollectionsStore } from "../collections";
  import { focusTrap } from "./focus-trap";

  let {
    store,
    assetIds,
    onClose,
  }: {
    store: CollectionsStore;
    /** 追加対象のstable AssetId列。 */
    assetIds: string[];
    onClose: () => void;
  } = $props();

  let storeTick = $state(0);
  const stopNotify = store.onChange(() => {
    storeTick++;
  });

  onDestroy(() => {
    stopNotify();
  });

  function manualCollections(): CollectionDto[] {
    void storeTick;
    return store.collections.filter((collection) => collection.kind === "manual");
  }

  let newName = $state("");
  let busy = $state(false);
  let notice = $state("");
  let doneNotice = $state("");

  function handleKeydown(event: KeyboardEvent): void {
    if (event.key === "Escape") {
      event.preventDefault();
      onClose();
    }
  }

  function handleBackdropPointerDown(event: PointerEvent): void {
    if (event.target === event.currentTarget) {
      onClose();
    }
  }

  async function addTo(collectionId: string, label: string): Promise<void> {
    if (busy || assetIds.length === 0) {
      return;
    }
    busy = true;
    notice = "";
    try {
      await store.addItems(collectionId, assetIds);
      doneNotice = `${label} へ ${assetIds.length.toLocaleString()}件を追加しました`;
    } catch (error) {
      notice = error instanceof Error ? error.message : String(error);
    } finally {
      busy = false;
    }
  }

  async function addToNew(): Promise<void> {
    const name = newName.trim();
    if (busy || name === "" || assetIds.length === 0) {
      return;
    }
    busy = true;
    notice = "";
    try {
      const created = await store.createManual(name);
      await store.addItems(created.id, assetIds);
      doneNotice = `「${name}」を作成し ${assetIds.length.toLocaleString()}件を追加しました`;
      newName = "";
    } catch (error) {
      notice = error instanceof Error ? error.message : String(error);
    } finally {
      busy = false;
    }
  }
</script>

<svelte:window onkeydown={handleKeydown} />

<div
  class="picker-backdrop"
  onpointerdown={handleBackdropPointerDown}
  role="presentation"
>
  <div class="picker" role="dialog" aria-modal="true" aria-label="コレクションへ追加" use:focusTrap>
    <h2>コレクションへ追加</h2>
    <p class="picker-sub">
      選択 {assetIds.length.toLocaleString()}件を追加します
      (元画像は移動しません — コレクションは仮想分類です)
    </p>

    {#if doneNotice}
      <p class="picker-done" role="status">{doneNotice}</p>
    {/if}
    {#if notice}
      <p class="picker-error" role="alert">{notice}</p>
    {/if}

    <div class="picker-list" role="list">
      {#each manualCollections() as collection (collection.id)}
        <button
          class="picker-row"
          disabled={busy}
          onclick={() => void addTo(collection.id, `「${collection.name}」`)}
        >
          <span class="picker-kind" aria-hidden="true">▤</span>
          <span class="picker-name">{collection.name}</span>
          <span class="picker-count">{collection.item_count.toLocaleString()}件</span>
        </button>
      {:else}
        <p class="picker-empty">手動コレクションがまだありません。下から新規作成できます。</p>
      {/each}
    </div>

    <div class="picker-new">
      <input
        placeholder="新規コレクション名"
        value={newName}
        oninput={(event) => (newName = event.currentTarget.value)}
        onkeydown={(event) => {
          if (event.key === "Enter") {
            void addToNew();
          }
        }}
      />
      <button disabled={busy || newName.trim() === ""} onclick={() => void addToNew()}>
        新規作成して追加
      </button>
    </div>
    <p class="picker-note">
      スマートコレクションは検索条件で自動的に構成されるため、手動追加はできません。
    </p>

    <div class="picker-actions">
      <button onclick={onClose}>閉じる</button>
    </div>
  </div>
</div>

<style>
  .picker-backdrop {
    position: fixed;
    inset: 0;
    z-index: 650;
    background: rgba(0, 0, 0, 0.4);
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .picker {
    width: min(28rem, 90vw);
    max-height: 80vh;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    background: light-dark(#ffffff, #1c2128);
    border: 1px solid light-dark(#d0d7de, #444c56);
    border-radius: 0.6rem;
    box-shadow: 0 12px 32px rgba(0, 0, 0, 0.35);
    padding: 0.9rem 1rem;
    font-size: 0.85rem;
  }

  .picker h2 {
    font-size: 1rem;
    margin: 0;
  }

  .picker-sub {
    margin: 0;
    font-size: 0.75rem;
    color: light-dark(#57606a, #8b949e);
  }

  .picker-done {
    margin: 0;
    font-size: 0.78rem;
    color: light-dark(#1a7f37, #3fb950);
  }

  .picker-error {
    margin: 0;
    font-size: 0.78rem;
    color: light-dark(#cf222e, #f85149);
    overflow-wrap: anywhere;
  }

  .picker-list {
    overflow-y: auto;
    min-height: 4rem;
    max-height: 18rem;
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
  }

  .picker-row {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    padding: 0.35rem 0.5rem;
    border: 1px solid light-dark(#d0d7de, #30363d);
    border-radius: 0.375rem;
    background: transparent;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
  }

  .picker-row:hover:not(:disabled) {
    background: light-dark(#ddf4ff, #121d2f);
  }

  .picker-kind {
    color: light-dark(#0969da, #58a6ff);
  }

  .picker-name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .picker-count {
    font-size: 0.72rem;
    color: light-dark(#57606a, #8b949e);
  }

  .picker-empty {
    margin: 0;
    font-size: 0.78rem;
    color: light-dark(#8b949e, #6e7681);
  }

  .picker-new {
    display: flex;
    gap: 0.4rem;
  }

  .picker-new input {
    flex: 1;
    min-width: 0;
    font: inherit;
    font-size: 0.8rem;
  }

  .picker-note {
    margin: 0;
    font-size: 0.7rem;
    color: light-dark(#8b949e, #6e7681);
  }

  .picker-actions {
    display: flex;
    justify-content: flex-end;
  }
</style>
