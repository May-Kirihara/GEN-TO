<script lang="ts">
  /**
    * フォルダツリーペイン (MVP WP-4)。
    *
    * - FolderTreeStoreの平坦行を描画 (遅延ロード・読み取り不能表示)
    * - 登録ルート (またはその配下) はバッジ表示
    * - クリックでフォルダ選択 (Appが登録/未登録でソースを切替)
    * - コレクション領域はWP-6でCollectionsPaneへ分離 (Appの左ペイン参照)
    * - WP-7: グリッドからのドラッグ&ドロップの受け口 (デフォルト=移動、
    *   Ctrl修飾=コピー。ツリーノードへドロップ)
    */
  import { onDestroy } from "svelte";
  import type { FolderTreeStore, TreeContextTarget } from "../fsview";
  import { registeredStateFor, treeContextTargetFor } from "../fsview";
  import type { LibraryRoot } from "../library";
  import ContextMenu, { type MenuAction } from "../ui/ContextMenu.svelte";

  let {
    tree,
    roots,
    currentFolder,
    libraryMode,
    onSelect,
    onDrop,
    onRegisterRoot,
    onUnregisterRoot,
    onBackupRoot,
    onRescan,
  }: {
    tree: FolderTreeStore;
    roots: LibraryRoot[];
    currentFolder: string | null;
    libraryMode: boolean;
    onSelect: (path: string) => void;
    /** グリッド選択のドロップ (WP-7)。destPathへ移動/コピー。 */
    onDrop:
      | ((destPath: string, paths: string[], copy: boolean) => void)
      | undefined;
    /** UIUX-1: 未登録フォルダを登録ルートへ追加 (library.addRoot経路)。 */
    onRegisterRoot: (path: string) => void;
    /** UIUX-1: 登録ルートを解除。 */
    onUnregisterRoot: (rootId: string) => void;
    /** UIUX-1: ルート単位の.gentoバックアップダイアログ (WP-13)。 */
    onBackupRoot: (root: LibraryRoot) => void;
    /** UIUX-1: ライブラリ全体の再スキャン (per-root APIは無く全体スキャンで代用)。 */
    onRescan: () => void;
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

  /** WP-7: ドロップ先として意味があるか (登録ルート配下のフォルダのみ)。 */
  function droppable(path: string): boolean {
    if (onDrop === undefined) {
      return false;
    }
    const state = registeredStateFor(path, roots);
    return state === "root" || state === "inside";
  }

  function extractPaths(event: DragEvent): string[] {
    const raw = event.dataTransfer?.getData("application/x-gento-paths");
    if (raw === "" || raw === undefined) {
      return [];
    }
    try {
      const parsed: unknown = JSON.parse(raw);
      if (!Array.isArray(parsed)) {
        return [];
      }
      return parsed.filter((item): item is string => typeof item === "string");
    } catch {
      return [];
    }
  }

  let dropTargetPath = $state<string | null>(null);

  function handleDragOver(event: DragEvent, path: string): void {
    if (!droppable(path)) {
      return;
    }
    const hasPayload =
      event.dataTransfer?.types.includes("application/x-gento-paths") ?? false;
    if (!hasPayload) {
      return;
    }
    event.preventDefault();
    if (event.dataTransfer !== null) {
      // Ctrl修飾 = コピー (MVPのD&D規約)
      event.dataTransfer.dropEffect = event.ctrlKey || event.metaKey ? "copy" : "move";
    }
    dropTargetPath = path;
  }

  function handleDragLeave(path: string): void {
    if (dropTargetPath === path) {
      dropTargetPath = null;
    }
  }

  function handleDrop(event: DragEvent, path: string): void {
    dropTargetPath = null;
    if (!droppable(path) || onDrop === undefined) {
      return;
    }
    const paths = extractPaths(event);
    if (paths.length === 0) {
      return;
    }
    event.preventDefault();
    onDrop(path, paths, event.ctrlKey || event.metaKey);
  }

  // ===== UIUX-1: 右クリックコンテキストメニュー (R-D 3状態) =====

  /** 開いているメニューの不変スナップショット (クリック時点のpath/rootId)。 */
  let treeMenu = $state<{ x: number; y: number; target: TreeContextTarget } | null>(null);

  $effect(() => {
    // roots更新でスナップショットがstale (状態kindが変化) したら閉じる。
    // 無関係なroots変化 (他ルートの追加) では閉じない。
    void roots;
    const open = treeMenu;
    if (open === null) {
      return;
    }
    if (treeContextTargetFor(open.target.path, roots).kind !== open.target.kind) {
      treeMenu = null;
    }
  });

  function handleRowContextMenu(event: MouseEvent, path: string): void {
    // WebView既定のコンテキストメニューを出さない。伝播も止めて
    // 単一ノードのメニューだけを開く (行入れ子・パネル境界の二重発火防止)。
    event.preventDefault();
    event.stopPropagation();
    const target = treeContextTargetFor(path, roots);
    if (target.kind === "descendant") {
      // 登録ルート配下: 追加・解除の対象外。ナビは通常クリック (既存) のみ
      return;
    }
    treeMenu = { x: event.clientX, y: event.clientY, target };
  }

  function menuActionsOf(target: TreeContextTarget): MenuAction[] {
    if (target.kind === "unrelated") {
      return [
        {
          id: "register",
          label: "このフォルダーを登録ルートに追加",
          title: "ライブラリへ登録して走査・編集可能にします",
          run: () => onRegisterRoot(target.path),
        },
      ];
    }
    if (target.kind === "root") {
      const rootId = target.rootId;
      return [
        {
          id: "unregister",
          label: "登録を解除",
          title: "このルートの登録をDBから解除します (ファイルは影響しません)",
          run: () => onUnregisterRoot(rootId),
        },
        {
          id: "backup",
          label: "このルートをバックアップ",
          title: "このルートのメタデータを.gentoバックアップへ書き出します",
          run: () => {
            const root = roots.find((entry) => entry.id === rootId);
            if (root !== undefined) {
              onBackupRoot(root);
            }
          },
        },
        {
          id: "rescan",
          label: "ライブラリを再スキャン",
          title: "登録ルート全体の再スキャンを開始します (per-rootスキャンではなく全体)",
          run: onRescan,
        },
      ];
    }
    return [];
  }
</script>

<div class="tree-pane">
  <div class="tree-scroll" role="tree" aria-label="フォルダーツリー">
    {#each rows() as row (row.path)}
      <div
        class="tree-row"
        class:row-selected={isCurrent(row.path)}
        class:row-error={row.error !== null}
        class:row-drop-target={dropTargetPath === row.path}
        style="padding-left:{0.35 + row.depth * 0.9}rem"
        role="treeitem"
        aria-expanded={row.hasChildren === false ? undefined : row.expanded}
        aria-selected={isCurrent(row.path)}
        tabindex={-1}
        ondragover={(event) => handleDragOver(event, row.path)}
        ondragleave={() => handleDragLeave(row.path)}
        ondrop={(event) => handleDrop(event, row.path)}
        oncontextmenu={(event) => handleRowContextMenu(event, row.path)}
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

{#if treeMenu !== null}
  <ContextMenu
    x={treeMenu.x}
    y={treeMenu.y}
    actions={menuActionsOf(treeMenu.target)}
    onClose={() => (treeMenu = null)}
  />
{/if}

<style>
  .tree-pane {
    display: flex;
    flex-direction: column;
    /* UIUX-1: 左ペインはタブ化され本コンポーネントはパネル単独の子。
     * 内部スクロール (.tree-scroll) が高さを握る。 */
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

  /* WP-7: ドロップ先ハイライト */
  .tree-row.row-drop-target {
    outline: 2px dashed light-dark(#0969da, #58a6ff);
    outline-offset: -2px;
    border-radius: 0.25rem;
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
