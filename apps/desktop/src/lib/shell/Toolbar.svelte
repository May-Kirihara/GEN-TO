<script lang="ts">
  /**
    * センターペイン上部ツールバー (UIUX-5/WP7bでApp.svelteから抽出)。
    * 状態・storeはApp残置 — 素値をpropsで受け取り操作はcallbackで返す
    * (WP7a契約と同一)。popover閉じaction (collectionToolsPopover) と
    * パス入力のautofocus actionは要素と同じcomponent内にあるためここへ移設。
    */
  import type { CatalogSort } from "../catalog";
  import type { CollectionDto } from "../collections";
  import type { Crumb } from "../browse";

  let {
    searchActive,
    searchRunning,
    searchKind,
    searchResultTotal,
    searchResultElapsedMs,
    onRefreshSearch,
    onExitSearch,
    activeCollection,
    onExitCollection,
    catalogSource,
    crumbs,
    onSelectFolder,
    pathInputOpen,
    pathInputValue,
    onOpenPathInput,
    onPathInput,
    onCommitPathInput,
    onCancelPathInput,
    sortValue,
    onSortChange,
    collectionToolsOpen,
    onToggleCollectionTools,
    onCloseCollectionTools,
    collectionBusy,
    selectionCount,
    collectionPositionInput,
    onCollectionPositionInput,
    onMoveSelectedInCollection,
    onRemoveSelectedFromCollection,
    onRefreshActiveSmartCollection,
    fileOpsBusy,
    onStartNewFolder,
    sizeValue,
    onSizeSlider,
    viewerChoice,
    onViewerChoice,
    onOpenViewer,
  }: {
    searchActive: boolean;
    searchRunning: boolean;
    searchKind: "query" | "similar";
    searchResultTotal: number;
    searchResultElapsedMs: number;
    onRefreshSearch: () => Promise<void>;
    onExitSearch: () => Promise<void>;
    activeCollection: CollectionDto | null;
    onExitCollection: () => Promise<void>;
    catalogSource: "temp" | "library";
    crumbs: Crumb[];
    onSelectFolder: (path: string) => Promise<boolean>;
    /** パンくずのパス入力モード (state正本はApp)。 */
    pathInputOpen: boolean;
    pathInputValue: string;
    onOpenPathInput: () => void;
    onPathInput: (value: string) => void;
    onCommitPathInput: () => void;
    onCancelPathInput: () => void;
    sortValue: CatalogSort;
    onSortChange: (sort: CatalogSort) => void;
    /** コレクション操作popoverの開閉state (正本はApp。showCollection/exitCollectionでreset)。 */
    collectionToolsOpen: boolean;
    onToggleCollectionTools: () => void;
    onCloseCollectionTools: () => void;
    collectionBusy: boolean;
    selectionCount: number;
    collectionPositionInput: string;
    onCollectionPositionInput: (value: string) => void;
    onMoveSelectedInCollection: (target: "front" | "back" | "position") => Promise<void>;
    onRemoveSelectedFromCollection: () => Promise<void>;
    onRefreshActiveSmartCollection: () => Promise<void>;
    fileOpsBusy: boolean;
    onStartNewFolder: () => Promise<void>;
    /** 0=小 (s256) / 1=大 (s512)。 */
    sizeValue: number;
    onSizeSlider: (value: number) => void;
    viewerChoice: "single" | "compare2" | "compare4";
    onViewerChoice: (choice: "single" | "compare2" | "compare4") => void;
    /** 選択フォーカスがあればビューア、無ければnotice (判断はApp側)。 */
    onOpenViewer: () => void;
  } = $props();

  /** popoverの外クリック判定用 (コレクション操作トグルボタン)。 */
  let collectionToolsToggleEl: HTMLElement | null = $state(null);

  /**
    * UIUX-5/WP3: コレクション操作popoverの閉じ取りまとめ (WP2/WP3と同じ
    * pointerdown capture+Escapeパターン)。popover内容はDOM常駐
    * (display:none切替) のため、リスナーは開いている間のみ作用する。
    */
  function collectionToolsPopover(node: HTMLElement): { destroy: () => void } {
    function onPointerDown(event: PointerEvent): void {
      if (!collectionToolsOpen) {
        return;
      }
      const target = event.target;
      if (
        target instanceof Node &&
        (node.contains(target) || collectionToolsToggleEl?.contains(target) === true)
      ) {
        return;
      }
      onCloseCollectionTools();
    }
    function onKeydown(event: KeyboardEvent): void {
      if (!collectionToolsOpen) {
        return;
      }
      if (event.key === "Escape") {
        event.preventDefault();
        onCloseCollectionTools();
      }
    }
    window.addEventListener("pointerdown", onPointerDown, true);
    window.addEventListener("keydown", onKeydown, true);
    return {
      destroy: () => {
        window.removeEventListener("pointerdown", onPointerDown, true);
        window.removeEventListener("keydown", onKeydown, true);
      },
    };
  }

  /** パンくずのパス入力をDOM挿入時にフォーカス・全選択 (Svelte action)。 */
  function autofocusSelect(node: HTMLInputElement): void {
    node.focus();
    node.select();
  }
</script>

<div class="toolbar">
  <nav class="breadcrumb" aria-label="現在場所">
    {#if searchActive}
      <span class="coll-current">
        <span class="coll-current-kind" aria-hidden="true">
          {searchKind === "similar" ? "◈" : "⌕"}
        </span>
        {searchKind === "similar" ? "類似画像" : "検索結果"}
        <span class="coll-current-count" data-testid="search-status">
          ({searchResultTotal.toLocaleString()}件・
          {searchResultElapsedMs.toLocaleString()}ms)
        </span>
      </span>
      <button
        class="crumb"
        title="検索条件を新しいDB状態で再実行"
        onclick={() => void onRefreshSearch()}
        disabled={searchRunning}
      >
        再実行
      </button>
      <button
        class="crumb"
        title="通常のライブラリ表示へ戻る"
        onclick={() => void onExitSearch()}
      >
        閉じる
      </button>
    {:else if activeCollection !== null}
      <span class="coll-current">
        <span class="coll-current-kind" aria-hidden="true">
          {activeCollection.kind === "smart" ? "✦" : "▤"}
        </span>
        コレクション「{activeCollection.name}」
        <span class="coll-current-count">
          ({activeCollection.item_count.toLocaleString()}件
          {activeCollection.kind === "smart" ? "・評価結果" : ""})
        </span>
      </span>
      <button
        class="crumb"
        title="通常のライブラリ表示へ戻る"
        onclick={() => void onExitCollection()}
      >
        閉じる
      </button>
    {:else if catalogSource === "library"}
      <span
        class="crumb-current"
        title="ライブラリ表示は登録ルート全体を表示します (フォルダーの絞り込みは検索scopeで実行できます)"
      >
        ライブラリ (登録ルート全体)
      </span>
      <span class="crumb-hint" title="フォルダー単位の絞り込みは検索scope (フォルダ) で実行できます">
        絞り込みは検索scope
      </span>
    {:else}
      {#each crumbs as crumb, index (crumb.path)}
        {#if index > 0}<span class="crumb-sep">›</span>{/if}
        <button class="crumb" onclick={() => void onSelectFolder(crumb.path)}>
          {crumb.name}
        </button>
      {/each}
      <!-- UIUX-1 (R-E): パス直接入力。Enterで移動・ESCで取消。 -->
      {#if pathInputOpen}
        <input
          class="crumb-input"
          data-testid="breadcrumb-path-input"
          type="text"
          aria-label="移動先フォルダーの絶対パス"
          placeholder="/絶対パス (Enterで移動・Escで取消)"
          value={pathInputValue}
          oninput={(event) => onPathInput(event.currentTarget.value)}
          onkeydown={(event) => {
            if (event.key === "Enter") {
              event.preventDefault();
              onCommitPathInput();
            } else if (event.key === "Escape") {
              event.preventDefault();
              onCancelPathInput();
            }
          }}
          onblur={() => onCancelPathInput()}
          use:autofocusSelect
        />
      {:else}
        <button
          class="crumb crumb-path-button"
          data-testid="breadcrumb-path-button"
          aria-label="パスを入力して移動"
          title="パスを入力して移動 (絶対パス)"
          onclick={() => onOpenPathInput()}
        >
          ✏
        </button>
      {/if}
    {/if}
  </nav>
  {#if activeCollection === null}
    <label class="sort-label">
      ソート
      <select
        value={sortValue}
        onchange={(event) => onSortChange(event.currentTarget.value as CatalogSort)}
      >
        <option value="name">名前</option>
        <option value="date_desc">更新日時</option>
        <option value="size_desc">容量</option>
      </select>
    </label>
  {:else if activeCollection.kind === "manual"}
    <!-- UIUX-5/WP3: 並び替え4操作は「…」オーバーフローpopoverへ集約。
         内容はDOM常駐+hidden切替 (devtest collMoveFront/BackがclickButton
         =DOM駆動のため。左ペインタブの全パネル常駐precedent)。 -->
    <span class="collection-overflow">
      <button
        bind:this={collectionToolsToggleEl}
        class="collection-overflow-toggle"
        class:active={collectionToolsOpen}
        aria-label="コレクション操作"
        aria-expanded={collectionToolsOpen}
        aria-haspopup="true"
        title="選択項目の並び替え・位置指定移動・コレクションから除外"
        onclick={() => onToggleCollectionTools()}
      >
        …
      </button>
      <div
        class="collection-popover"
        class:hidden={!collectionToolsOpen}
        role="group"
        aria-label="コレクション内の並び替え"
        use:collectionToolsPopover
      >
        <button
          disabled={collectionBusy || selectionCount === 0}
          title="選択項目をコレクションの先頭へ移動"
          onclick={() => void onMoveSelectedInCollection("front")}
        >
          選択を先頭へ
        </button>
        <button
          disabled={collectionBusy || selectionCount === 0}
          title="選択項目をコレクションの末尾へ移動"
          onclick={() => void onMoveSelectedInCollection("back")}
        >
          選択を末尾へ
        </button>
        <span class="position-move">
          <input
            type="number"
            min="1"
            placeholder="位置"
            aria-label="移動先の位置 (1-based)"
            value={collectionPositionInput}
            oninput={(event) => onCollectionPositionInput(event.currentTarget.value)}
            onkeydown={(event) => {
              if (event.key === "Enter") {
                void onMoveSelectedInCollection("position");
              }
            }}
          />
          <button
            disabled={collectionBusy || selectionCount === 0}
            title="選択項目を指定位置へ移動"
            onclick={() => void onMoveSelectedInCollection("position")}
          >
            へ移動
          </button>
        </span>
        <button
          disabled={collectionBusy || selectionCount === 0}
          title="選択項目をこのコレクションから除外 (元画像は影響しません)"
          onclick={() => void onRemoveSelectedFromCollection()}
        >
          コレクションから除外
        </button>
      </div>
    </span>
  {:else}
    <span class="collection-tools" role="group" aria-label="スマートコレクション操作">
      <button
        disabled={collectionBusy}
        title="検索条件を再評価して内容を更新 (評価・タグ等の変更後)"
        onclick={() => void onRefreshActiveSmartCollection()}
      >
        再評価
      </button>
    </span>
  {/if}
  <!-- UIUX-5/WP3: toolbarエクスポートボタンは撤去。MenuBar
       「ファイル>SD学習用エクスポート…」が正本 (E2E契約) -->
  <button
    class="toolbar-folder"
    disabled={catalogSource !== "library" || fileOpsBusy}
    title="現在の登録ルートに新しいフォルダを作成"
    onclick={() => void onStartNewFolder()}
  >
    + 新規フォルダ
  </button>
  <div class="toolbar-spacer"></div>
  <!-- UIUX-5/WP3: 2値 (小=0/大=1) トグルへ置換。sizeSliderValue/
       onSizeSliderとthumbSize保存値のsemanticsは不変。button複数を
       label内に置けないためsize-labelはspan化 (文言・classは維持) -->
  <span class="size-label" title="サムネイルサイズ">
    サイズ
    <span class="size-toggle" role="group" aria-label="サムネイルサイズ">
      <button
        aria-pressed={sizeValue === 0}
        class:active={sizeValue === 0}
        onclick={() => onSizeSlider(0)}
      >
        小
      </button>
      <button
        aria-pressed={sizeValue === 1}
        class:active={sizeValue === 1}
        onclick={() => onSizeSlider(1)}
      >
        大
      </button>
    </span>
  </span>
  <!-- UIUX-3 V-1: スクロール速度スライダーはツールバーから撤去し
       設定dialog表示タブへ移設 (xsp=はdialog表示タブ展開時のみ読める) -->
  <span class="viewer-choice">
    <!-- UIUX-5/WP13 (Minor-7): 3トグルへaria-pressed付与 (size-toggleと同一
         様式)。文言・.active・button構造・順序はE2E契約で不変、「開く」は
         action buttonのため対象外。 -->
    <button
      aria-pressed={viewerChoice === "single"}
      class:active={viewerChoice === "single"}
      onclick={() => onViewerChoice("single")}
    >
      単一
    </button>
    <button
      aria-pressed={viewerChoice === "compare2"}
      class:active={viewerChoice === "compare2"}
      onclick={() => onViewerChoice("compare2")}
    >
      2枚比較
    </button>
    <button
      aria-pressed={viewerChoice === "compare4"}
      class:active={viewerChoice === "compare4"}
      onclick={() => onViewerChoice("compare4")}
    >
      4枚比較
    </button>
    <button
      onclick={() => onOpenViewer()}
    >
      開く
    </button>
  </span>
</div>

<style>
  .toolbar {
    flex: none;
    display: flex;
    align-items: center;
    gap: var(--space-60);
    padding: var(--space-30) var(--space-60);
    font-size: var(--font-size-80);
    flex-wrap: wrap;
  }

  .breadcrumb {
    display: flex;
    align-items: center;
    gap: var(--space-15);
    min-width: 0;
    overflow: hidden;
    flex-wrap: wrap;
  }

  .crumb {
    border: none;
    background: transparent;
    color: var(--color-accent);
    font: inherit;
    padding: 0.1rem 0.25rem;
    border-radius: var(--radius-xs);
    cursor: pointer;
  }

  .crumb:hover {
    background: var(--color-bg-subtle);
  }

  .crumb-current {
    font-weight: bold;
  }

  /* UXLIB-1: ライブラリ表示の現仕様 (全体表示・絞り込みなし) の一案内 */
  .crumb-hint {
    font-size: var(--font-size-70);
    color: var(--color-text-muted);
  }

  .coll-current {
    display: inline-flex;
    align-items: center;
    gap: var(--space-25);
    font-weight: bold;
  }

  .coll-current-kind {
    color: var(--color-accent);
  }

  .coll-current-count {
    font-weight: normal;
    font-size: var(--font-size-72);
    color: var(--color-text-muted);
  }

  .collection-tools {
    display: inline-flex;
    align-items: center;
    gap: var(--space-30);
    flex-wrap: wrap;
  }

  .position-move {
    display: inline-flex;
    align-items: center;
    gap: var(--space-20);
  }

  .position-move input {
    width: 4.5rem;
    font: inherit;
    font-size: var(--font-size-78);
  }

  /* UIUX-5/WP3: manual collection操作のオーバーフロー (popoverの位置基準) */
  .collection-overflow {
    position: relative;
    display: inline-flex;
    align-items: center;
  }

  .collection-overflow-toggle {
    font: inherit;
    font-size: var(--font-size-78);
    padding: var(--space-15) var(--space-60);
    border-radius: var(--radius-sm);
    border: 1px solid var(--color-border-strong);
    background: transparent;
    color: inherit;
    cursor: pointer;
  }

  /* DOM常駐+hidden切替のpopover (search-filtersと同様の浮遊様式) */
  .collection-popover {
    position: absolute;
    top: 100%;
    left: 0;
    z-index: 600;
    margin-top: var(--space-40);
    display: flex;
    flex-direction: column;
    align-items: stretch;
    gap: var(--space-30);
    min-width: 16rem;
    padding: var(--space-60) var(--space-75);
    font-size: var(--font-size-78);
    background: var(--color-surface-raised);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-popover);
  }

  .collection-popover.hidden {
    display: none;
  }

  .collection-popover button {
    font: inherit;
    font-size: var(--font-size-78);
    padding: var(--space-15) var(--space-50);
    border-radius: var(--radius-sm);
    border: 1px solid var(--color-border-strong);
    background: transparent;
    color: inherit;
    cursor: pointer;
    text-align: left;
  }

  .collection-popover button:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .crumb-sep {
    color: var(--color-text-subtle);
  }

  /* UIUX-1 (R-E): パンくずのパス直接入力 */
  .crumb-input {
    font: inherit;
    font-size: var(--font-size-78);
    font-family: monospace;
    min-width: 14rem;
    flex: 0 1 auto;
    padding: 0.1rem 0.3rem;
  }

  .crumb-path-button {
    font-size: var(--font-size-85);
    padding: 0.1rem 0.35rem;
  }

  .sort-label,
  .size-label {
    display: inline-flex;
    align-items: center;
    gap: var(--space-30);
    color: var(--color-text-muted);
    font-size: var(--font-size-78);
  }

  /* UIUX-5/WP3: サイズ2値 (小/大) のセグメントトグル。選択側は
     button.active (accessibility: aria-pressed) */
  .size-toggle {
    display: inline-flex;
    align-items: stretch;
  }

  .size-toggle button {
    border: 1px solid var(--color-border-strong);
    border-radius: 0;
    background: transparent;
    color: inherit;
    font: inherit;
    padding: var(--space-15) var(--space-50);
    cursor: pointer;
    position: relative;
  }

  .size-toggle button:first-of-type {
    border-radius: var(--radius-sm) 0 0 var(--radius-sm);
  }

  .size-toggle button:last-of-type {
    border-radius: 0 var(--radius-sm) var(--radius-sm) 0;
    margin-left: -1px;
  }

  .size-toggle button.active {
    z-index: 1;
  }

  .toolbar-spacer {
    flex: 1;
  }

  /* UIUX-5/WP3: ビューア選択3ボタンをセグメントコントロール化 (CSSのみ。
     文言・.active・button構造はE2E契約 (devtest cycleChoice/openViewer) で不変)。
     「開く」(4ボタン目) は分離したまま。 */
  .viewer-choice {
    display: inline-flex;
    align-items: stretch;
  }

  .viewer-choice button {
    border: 1px solid var(--color-border-strong);
    border-radius: 0;
    background: transparent;
    color: inherit;
    font: inherit;
    font-size: var(--font-size-78);
    padding: var(--space-15) var(--space-50);
    cursor: pointer;
    position: relative;
  }

  .viewer-choice button:first-of-type {
    border-radius: var(--radius-sm) 0 0 var(--radius-sm);
  }

  .viewer-choice button:nth-of-type(2),
  .viewer-choice button:nth-of-type(3) {
    margin-left: -1px;
  }

  .viewer-choice button:nth-of-type(3) {
    border-radius: 0 var(--radius-sm) var(--radius-sm) 0;
  }

  .viewer-choice button:nth-of-type(4) {
    margin-left: var(--space-30);
    border-radius: var(--radius-sm);
  }

  /* 選択中/hoverの枠は隣接ボタンとのborder重なり (margin -1px) に
     塗り潰されないよう手前に描画する */
  .viewer-choice button.active,
  .viewer-choice button:hover {
    z-index: 1;
  }

  /* App.svelteの全button共通則と同一値 (scoped CSSのためcomponent毎に必要)。
     本componentのclass-less buttonはsize/viewerトグルのみで、それぞれ
     font指定を持つため button 基本則は不要 — .activeのみ移設。 */
  button.active {
    border-color: var(--color-accent);
    font-weight: bold;
  }

  .toolbar-folder {
    font: inherit;
    font-size: var(--font-size-78);
    padding: var(--space-15) var(--space-50);
    border-radius: var(--radius-sm);
    border: 1px solid var(--color-border-strong);
    background: transparent;
    color: inherit;
    cursor: pointer;
  }

  .toolbar-folder:disabled {
    opacity: 0.5;
    cursor: default;
  }
</style>
