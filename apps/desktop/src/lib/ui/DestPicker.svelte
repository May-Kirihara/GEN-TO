<script lang="ts">
  /**
    * コピー/移動の宛先フォルダ選択ダイアログ (MVP WP-7)。
    *
    * 登録ルート起点のディレクトリツリー (fs_list_dirsで遅延ロード) から
    * 宛先を選ぶ (フォルダツリー選択式)。新規サブフォルダの作成も可能。
    */
  import { invoke } from "@tauri-apps/api/core";
  import type { LibraryRoot } from "../library";
  import { focusTrap } from "./focus-trap";

  let {
    roots,
    title,
    confirmLabel,
    onPick,
    onClose,
  }: {
    roots: LibraryRoot[];
    title: string;
    confirmLabel: string;
    onPick: (destDir: string) => void;
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
  // (開いている間にルートが増減することは稀。svelte-checkの初期値捕捉
  // 警告は意図的 — 変更は次回オープンで反映される)。
  let nodes = $state<Node[]>(
    roots
      .filter((root) => root.available)
      .map((root) => ({
        path: root.path,
        name: root.path,
        expanded: false,
        children: null,
        loading: false,
      })),
  );
  let selected = $state<string | null>(null);
  let notice = $state("");
  let newFolderName = $state("");

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
    } catch (error) {
      node.children = [];
      notice = error instanceof Error ? error.message : String(error);
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
  }

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

  async function createSubfolder(): Promise<void> {
    if (selected === null) {
      notice = "親フォルダを先に選んでください";
      return;
    }
    const name = newFolderName.trim();
    if (name === "") {
      notice = "フォルダ名を入力してください";
      return;
    }
    notice = "";
    try {
      const outcome = await invoke<{ status: string; steps: Array<{ error: string | null }> }>(
        "folder_create",
        { parent: selected, name },
      );
      if (outcome.status !== "done") {
        notice = outcome.steps[0]?.error ?? "フォルダを作成できませんでした";
        return;
      }
      newFolderName = "";
      // 選択を作成済みフォルダへ進める
      selected = selected === "" ? name : `${selected.replace(/\/+$/, "")}/${name}`;
      // 展開中の親があれば再読込 (簡単化: 選択のみ更新)
      for (const node of visitAll(nodes)) {
        if (node.path === selected && node.children === null) {
          void loadChildren(node);
        }
      }
      notice = `「${name}」を作成しました`;
    } catch (error) {
      notice = error instanceof Error ? error.message : String(error);
    }
  }

  function* visitAll(list: Node[]): Generator<Node> {
    for (const node of list) {
      yield node;
      if (node.children !== null) {
        yield* visitAll(node.children);
      }
    }
  }
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="picker-backdrop" onpointerdown={handleBackdropPointerDown} role="presentation">
  <div class="picker" role="dialog" aria-modal="true" aria-label={title} use:focusTrap>
    <h2>{title}</h2>
    <p class="picker-sub">宛先フォルダを選択してください (登録ルート内のフォルダツリー)</p>
    {#if notice}
      <p class="picker-error" role="alert">{notice}</p>
    {/if}

    <div class="tree" role="tree">
      {#each nodes as node (node.path)}
        {@render renderNode(node, 0)}
      {/each}
    </div>

    <div class="new-folder">
      <input
        placeholder="選択中フォルダの下に新規作成"
        value={newFolderName}
        oninput={(event) => (newFolderName = event.currentTarget.value)}
        onkeydown={(event) => {
          if (event.key === "Enter") {
            void createSubfolder();
          }
        }}
      />
      <button disabled={newFolderName.trim() === ""} onclick={() => void createSubfolder()}>
        フォルダ作成
      </button>
    </div>

    <p class="picker-path" data-testid="dest-selected">選択中: {selected ?? "—"}</p>

    <div class="picker-actions">
      <button onclick={onClose}>キャンセル</button>
      <button
        class="primary"
        disabled={selected === null}
        onclick={() => selected !== null && onPick(selected)}
      >
        {confirmLabel}
      </button>
    </div>
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
    width: min(34rem, 90vw);
    max-height: 80vh;
    display: flex;
    flex-direction: column;
    gap: var(--space-50);
    background: var(--color-surface-raised);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-xl);
    box-shadow: 0 12px 32px rgba(0, 0, 0, 0.35);
    padding: var(--space-90) var(--space-100);
    font-size: var(--font-size-85);
  }

  .picker h2 {
    font-size: var(--font-size-100);
    margin: 0;
  }

  .picker-sub {
    margin: 0;
    font-size: var(--font-size-75);
    color: var(--color-text-muted);
  }

  .picker-error {
    margin: 0;
    font-size: var(--font-size-80);
    color: var(--color-danger);
    overflow-wrap: anywhere;
  }

  .tree {
    overflow-y: auto;
    min-height: 6rem;
    max-height: 22rem;
    border: 1px solid var(--color-border-strong);
    border-radius: var(--radius-sm);
    padding: var(--space-25);
  }

  .node-row {
    display: flex;
    align-items: center;
    gap: var(--space-15);
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
    padding: var(--space-15) var(--space-30);
    border-radius: var(--radius-xs);
    cursor: pointer;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .node-label:hover {
    background: var(--color-bg-subtle);
  }

  .node-selected {
    background: var(--color-accent-subtle);
    font-weight: 600;
  }

  .new-folder {
    display: flex;
    gap: var(--space-40);
  }

  .new-folder input {
    flex: 1;
    min-width: 0;
    font: inherit;
    font-size: var(--font-size-80);
  }

  .picker-path {
    margin: 0;
    font-size: var(--font-size-75);
    color: var(--color-text-muted);
    overflow-wrap: anywhere;
  }

  .picker-actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-40);
  }

  .primary {
    background: var(--color-accent-emphasis);
    color: #ffffff;
    border: none;
    border-radius: var(--radius-sm);
    padding: var(--space-30) var(--space-80);
    cursor: pointer;
  }

  .primary:disabled {
    opacity: 0.5;
    cursor: default;
  }
</style>
