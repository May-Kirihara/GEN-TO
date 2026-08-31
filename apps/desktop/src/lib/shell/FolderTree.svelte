<script lang="ts">
  /**
    * フォルダツリーペイン (MVP WP-4)。
    *
    * - FolderTreeStoreの平坦行を描画 (遅延ロード・読み取り不能表示)
    * - 登録ルート (またはその配下) はバッジ表示
    * - クリックでフォルダ選択 (Appが登録/未登録でソースを切替)
    * - コレクション領域はWP-6でCollectionsPaneへ分離 (Appの左ペイン参照)
    */
  import { onDestroy } from "svelte";
  import type { FolderTreeStore } from "../fsview";
  import { registeredStateFor } from "../fsview";
  import type { LibraryRoot } from "../library";

  let {
    tree,
    roots,
    currentFolder,
    libraryMode,
    onSelect,
  }: {
    tree: FolderTreeStore;
    roots: LibraryRoot[];
    /** 現在閲覧中フォルダ (未登録表示時)。 */
    currentFolder: string | null;
    libraryMode: boolean;
    onSelect: (path: string) => void;
  } = $props();

  let treeTick = $state(0);
  // svelte-ignore state_referenced_locally
  const stopNotify = tree.onChange(() => {
    treeTick++;
  });

  onDestroy(() => {
    stopNotify();
  });

  function rows() {
    void treeTick;
    return tree.visibleRows();
  }

  function badgeOf(path: string): string | null {
    void treeTick;
    const state = registeredStateFor(path, roots);
    if (state === "root") {
      return "登録ルート";
    }
    if (state === "inside") {
      return "登録内";
    }
    return null;
  }

  function isCurrent(path: string): boolean {
    void treeTick;
    return !libraryMode && currentFolder === path;
  }
</script>

<div class="tree-pane">
  <div class="tree-scroll" role="tree" aria-label="フォルダーツリー">
    {#each rows() as row (row.path)}
      <div
        class="tree-row"
        class:row-selected={isCurrent(row.path)}
        class:row-error={row.error !== null}
        style="padding-left:{0.35 + row.depth * 0.9}rem"
        role="treeitem"
        aria-expanded={row.hasChildren === false ? undefined : row.expanded}
        aria-selected={isCurrent(row.path)}
      >
        <button
          class="tree-expander"
          aria-label={row.expanded ? "折りたたむ" : "展開する"}
          onclick={() => void tree.toggle(row.path)}
        >
          {#if row.hasChildren === false}
            <span class="tree-dot"></span>
          {:else if row.expanded}
            <span aria-hidden="true">▾</span>
          {:else}
            <span aria-hidden="true">▸</span>
          {/if}
        </button>
        <button
          class="tree-label"
          title={row.path}
          onclick={() => onSelect(row.path)}
        >
          <span class="tree-name">{row.name}</span>
          {#if badgeOf(row.path) !== null}
            <span class="tree-badge">{badgeOf(row.path)}</span>
          {/if}
          {#if row.loading}
            <span class="tree-loading">…</span>
          {/if}
        </button>
      </div>
      {#if row.error !== null}
        <div
          class="tree-error-row"
          style="padding-left:{1.5 + row.depth * 0.9}rem"
        >
          読み取り不能: {row.error}
        </div>
      {/if}
    {/each}
    {#if rows().length === 0}
      <div class="tree-empty">
        {tree.lastError ?? "ツリーを読み込み中…"}
      </div>
    {/if}
  </div>

  {#if libraryMode}
    <p class="tree-note">ライブラリ表示中 (登録ルート全体)</p>
  {/if}
</div>

<style>
  .tree-pane {
    display: flex;
    flex-direction: column;
    /* 左ペインはFolderTree + CollectionsPane + LibraryPanelの3子なので
     * height:100%でなくflex配分にする (WP-6)。 */
    flex: 1 1 auto;
    min-height: 6rem;
  }

  .tree-scroll {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    overflow-x: hidden;
    font-size: 0.875rem;
    padding: 0.25rem 0;
  }

  .tree-row {
    display: flex;
    align-items: center;
    gap: 0.15rem;
    white-space: nowrap;
  }

  .tree-row.row-selected {
    background: light-dark(#ddf4ff, #121d2f);
  }

  .tree-row.row-error .tree-name {
    color: light-dark(#8b949e, #6e7681);
  }

  .tree-expander {
    flex: none;
    width: 1.1rem;
    height: 1.3rem;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border: none;
    background: transparent;
    color: inherit;
    cursor: pointer;
    font-size: 0.7rem;
  }

  .tree-dot {
    width: 0.25rem;
    height: 0.25rem;
    border-radius: 50%;
    background: currentColor;
    opacity: 0.4;
  }

  .tree-label {
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
    border: none;
    background: transparent;
    color: inherit;
    font: inherit;
    padding: 0.15rem 0.35rem 0.15rem 0;
    border-radius: 0.25rem;
    cursor: pointer;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 100%;
  }

  .tree-label:hover {
    background: light-dark(#eaeef2, #21262d);
  }

  .tree-badge {
    flex: none;
    font-size: 0.65rem;
    padding: 0.05rem 0.3rem;
    border-radius: 0.5rem;
    background: light-dark(#1a7f37, #238636);
    color: #ffffff;
  }

  .tree-loading {
    color: light-dark(#57606a, #8b949e);
  }

  .tree-error-row {
    font-size: 0.75rem;
    color: light-dark(#cf222e, #f85149);
    padding-top: 0.1rem;
    padding-bottom: 0.2rem;
    white-space: nowrap;
  }

  .tree-empty {
    padding: 0.75rem;
    color: light-dark(#57606a, #8b949e);
    font-size: 0.85rem;
  }

  .tree-note {
    margin: 0.25rem 0.5rem;
    font-size: 0.75rem;
    color: light-dark(#57606a, #8b949e);
  }
</style>
