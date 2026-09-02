<script lang="ts">
  /**
    * ポータブルバックアップ作成ダイアログ (MVP WP-13)。
    *
    * - 保存先ディレクトリ選択 (テキスト直接入力 + フォルダツリー)。
    *   既定値は backendの `backup_default_dir` (db-backups/)。
    * - 作成は `backup_portable_create` (ルート単位の .gento ディレクトリ)。
    */
  import { invoke } from "@tauri-apps/api/core";
  import type { LibraryRoot } from "../library";
  import type { PortableCreatedDto } from "../backup";

  let {
    root,
    roots,
    onClose,
  }: {
    root: LibraryRoot;
    roots: LibraryRoot[];
    onClose: () => void;
  } = $props();

  type Node = {
    path: string;
    name: string;
    expanded: boolean;
    children: Node[] | null;
    loading: boolean;
  };

  // 短命なモーダル: オープン時点のルート一覧から固定で構築する
  // (DestPickerと同じ規約 — 変更は次回オープンで反映。svelte-checkの
  // 初期値捕捉警告は意図的)。
  let nodes = $state<Node[]>(
    roots
      .filter((r) => r.available)
      .map((r) => ({
        path: r.path,
        name: r.path,
        expanded: false,
        children: null,
        loading: false,
      })),
  );
  let selected = $state<string | null>(null);
  let destInput = $state("");
  let busy = $state(false);
  let error = $state<string | null>(null);
  let created = $state<PortableCreatedDto | null>(null);

  $effect(() => {
    void (async () => {
      try {
        destInput = await invoke<string>("backup_default_dir");
      } catch {
        destInput = "";
      }
    })();
  });

  async function loadChildren(node: Node): Promise<void> {
    if (node.children !== null || node.loading) {
      return;
    }
    node.loading = true;
    try {
      const list = await invoke<{ entries: Array<{ name: string; path: string }> }>(
        "fs_list_dirs",
        { path: node.path },
      );
      node.children = list.entries.map((entry) => ({
        path: entry.path,
        name: entry.name,
        expanded: false,
        children: null,
        loading: false,
      }));
    } catch (err) {
      node.children = [];
      error = err instanceof Error ? err.message : String(err);
    } finally {
      node.loading = false;
    }
  }

  function toggle(node: Node): void {
    node.expanded = !node.expanded;
    if (node.expanded) {
      void loadChildren(node);
    }
  }

  function select(node: Node): void {
    selected = node.path;
    destInput = node.path;
  }

  function handleKeydown(event: KeyboardEvent): void {
    if (event.key === "Escape") {
      event.preventDefault();
      onClose();
    }
  }

  async function create(): Promise<void> {
    if (destInput.trim() === "") {
      error = "保存先ディレクトリを入力または選択してください";
      return;
    }
    busy = true;
    error = null;
    try {
      created = await invoke<PortableCreatedDto>("backup_portable_create", {
        rootId: root.id,
        destDir: destInput.trim(),
      });
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  function handleBackdropPointerDown(event: PointerEvent): void {
    if (event.target === event.currentTarget) {
      onClose();
    }
  }
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="picker-backdrop" onpointerdown={handleBackdropPointerDown} role="presentation">
  <div class="picker" role="dialog" aria-modal="true" aria-label="バックアップ作成">
    <h2>バックアップ作成</h2>
    {#if created}
      <p class="picker-sub" data-testid="backup-created">
        バックアップを作成しました:
        <span class="mono">{created.path}</span><br />
        画像 {created.assetCount.toLocaleString()}件 /
        ファイル {created.fileCount.toLocaleString()}件 /
        コレクション {created.collectionCount.toLocaleString()}件
      </p>
      <div class="picker-actions">
        <button class="primary" onclick={onClose}>閉じる</button>
      </div>
    {:else}
      <p class="picker-sub">
        ルート <span class="mono">{root.path}</span> のメタデータを
        `.gento` バックアップとして書き出します (画像ファイル本体は含みません)
      </p>
      {#if error}
        <p class="picker-error" role="alert">{error}</p>
      {/if}
      <label class="dest-label">
        保存先ディレクトリ
        <input
          type="text"
          data-testid="backup-dest"
          bind:value={destInput}
          placeholder="/path/to/backups"
        />
      </label>
      <div class="tree" role="tree">
        {#each nodes as node (node.path)}
          {@render renderNode(node, 0)}
        {/each}
      </div>
      <p class="picker-path" data-testid="backup-selected">入力中: {destInput || "—"}</p>
      <div class="picker-actions">
        <button onclick={onClose}>キャンセル</button>
        <button class="primary" disabled={busy} onclick={() => void create()}>
          {busy ? "作成中…" : "バックアップを作成"}
        </button>
      </div>
    {/if}
  </div>
</div>

{#snippet renderNode(node: Node, depth: number)}
  <div
    class="node-row"
    role="treeitem"
    aria-selected={selected === node.path}
    tabindex={-1}
    style="padding-left:{depth * 14 + 6}px"
  >
    <button
      class="node-toggle"
      aria-label={node.expanded ? "折りたたむ" : "展開する"}
      onclick={() => toggle(node)}
    >
      {node.children === null && node.loading ? "…" : node.expanded ? "▾" : "▸"}
    </button>
    <button
      class="node-label"
      class:node-selected={selected === node.path}
      onclick={() => select(node)}
      ondblclick={() => {
        toggle(node);
        select(node);
      }}
      title={node.path}
    >
      {node.name}
    </button>
  </div>
  {#if node.expanded && node.children !== null}
    {#each node.children as child (child.path)}
      {@render renderNode(child, depth + 1)}
    {/each}
  {/if}
{/snippet}

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
    width: min(36rem, 90vw);
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
    overflow-wrap: anywhere;
  }

  .mono {
    font-family: monospace;
    font-size: 0.72rem;
  }

  .picker-error {
    margin: 0;
    font-size: 0.78rem;
    color: light-dark(#cf222e, #f85149);
    overflow-wrap: anywhere;
  }

  .dest-label {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    font-size: 0.78rem;
  }

  .dest-label input {
    font: inherit;
    font-size: 0.8rem;
  }

  .tree {
    overflow-y: auto;
    min-height: 5rem;
    max-height: 16rem;
    border: 1px solid light-dark(#d0d7de, #30363d);
    border-radius: 0.375rem;
    padding: 0.25rem;
  }

  .node-row {
    display: flex;
    align-items: center;
    gap: 0.15rem;
  }

  .node-toggle {
    width: 1.2rem;
    border: none;
    background: transparent;
    color: inherit;
    cursor: pointer;
    padding: 0;
  }

  .node-label {
    flex: 1;
    min-width: 0;
    text-align: left;
    border: none;
    background: transparent;
    color: inherit;
    font: inherit;
    padding: 0.15rem 0.3rem;
    border-radius: 0.25rem;
    cursor: pointer;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .node-label:hover {
    background: light-dark(#eaeef2, #21262d);
  }

  .node-selected {
    background: light-dark(#ddf4ff, #121d2f);
    font-weight: 600;
  }

  .picker-path {
    margin: 0;
    font-size: 0.72rem;
    color: light-dark(#57606a, #8b949e);
    overflow-wrap: anywhere;
  }

  .picker-actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.4rem;
  }

  .primary {
    background: light-dark(#0969da, #1f6feb);
    color: #ffffff;
    border: none;
    border-radius: 0.375rem;
    padding: 0.3rem 0.8rem;
    cursor: pointer;
  }

  .primary:disabled {
    opacity: 0.5;
    cursor: default;
  }
</style>
