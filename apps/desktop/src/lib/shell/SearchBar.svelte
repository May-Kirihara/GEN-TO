<script lang="ts">
  /**
    * 検索行 (UIUX-5/WP7bでApp.svelteから抽出)。折りたたみ1行・展開行・
    * フィルタpopover・チップ行を含む (search-barのposition:relativeが
    * popoverの配置基準のため同一component内に置く)。
    * 状態・storeはApp残置 — 素値をpropsで受け取り操作はcallbackで返す。
    * searchFiltersPopover (閉じaction) は要素と同じcomponent内にあるため
    * ここへ移設 (window listenerのためscoped制約なし — WP7a契約踏襲)。
    */
  import { EMPTY_FILTERS, type GuiFilters } from "../search";
  import type { CollapsedSearchSummary } from "../ui/search-summary";
  import type { FilterChip, FilterChipKey } from "../ui/filter-chips";
  import type { SemanticMode } from "../ui/search-mode";

  let {
    searchExpanded,
    onToggleSearchExpanded,
    collapsedSearchLine,
    onClearSearchFromCollapsed,
    searchRunning,
    searchInput,
    onSearchInput,
    onRunSearch,
    searchScopeMode,
    onSearchScopeChange,
    semanticAvailable,
    semanticReason,
    semanticMode,
    onSemanticModeChange,
    showSemanticSuggest,
    onSemanticSuggestClick,
    searchFiltersOpen,
    onToggleSearchFilters,
    onCloseSearchFilters,
    searchFiltersActive,
    searchWarnings,
    canSaveSearch,
    onSaveCurrentSearch,
    searchFilters,
    onSearchFiltersChange,
    searchFilterChipList,
    onRemoveSearchFilterChip,
  }: {
    searchExpanded: boolean;
    onToggleSearchExpanded: () => void;
    /** 折りたたみ行の要約 (Appのderived — search-summary参照)。 */
    collapsedSearchLine: CollapsedSearchSummary;
    onClearSearchFromCollapsed: () => void;
    searchRunning: boolean;
    searchInput: string;
    onSearchInput: (value: string) => void;
    onRunSearch: () => void;
    searchScopeMode: "library" | "folder" | "collection";
    onSearchScopeChange: (mode: "library" | "folder" | "collection") => void;
    semanticAvailable: boolean;
    semanticReason: string;
    /** UIUX-7: 検索モード (3値 — 実行重みはAppがMODE_WEIGHT_PERCENTで導出)。 */
    semanticMode: SemanticMode;
    onSemanticModeChange: (mode: SemanticMode) => void;
    /** 意味検索サジェストチップの表示 (判定はApp — 検索実行後の確定値のみ)。 */
    showSemanticSuggest: boolean;
    /** チップclick → mode=hybrid設定+同クエリ再実行 (AppのtrySemanticSearch)。 */
    onSemanticSuggestClick: () => void;
    /** フィルタpopoverの開閉state (正本はApp — レイアウト系)。 */
    searchFiltersOpen: boolean;
    onToggleSearchFilters: () => void;
    onCloseSearchFilters: () => void;
    searchFiltersActive: boolean;
    /** 最終実行の警告列 (Appで searchStore.lastResult?.warnings ?? [] を計算済み)。 */
    searchWarnings: string[];
    /** 保存済み検索として保存できるか (searchStore.lastResult !== null)。 */
    canSaveSearch: boolean;
    onSaveCurrentSearch: () => void;
    searchFilters: GuiFilters;
    onSearchFiltersChange: (filters: GuiFilters) => void;
    searchFilterChipList: FilterChip[];
    onRemoveSearchFilterChip: (key: FilterChipKey) => void;
  } = $props();

  /** popoverの外クリック判定用 (トグルボタン・チップ行上では閉じない)。 */
  let searchFilterToggleEl: HTMLElement | null = $state(null);
  let searchFilterChipsEl: HTMLElement | null = $state(null);

  /**
    * UIUX-7/WP5 (QA Major): 意味検索不可の理由はボタン自身のtitleに機能説明へ
    * 改行連結して出す — セグメントspanのtitleは子buttonのtitleが優先され
    * ホバーで出ないため (旧スライダーの理由tooltip契約の復帰)。
    */
  function modeTitle(description: string): string {
    return semanticAvailable ? description : `${description}\n${semanticReason}`;
  }

  /**
    * UIUX-5/WP2: フィルタpopoverの閉じ取りまとめ (ContextMenu precedent)。
    * popover外のpointerdownとEscapeで閉じる。トグルボタン上は無視 — 外クリック
    * closeとclick反転が二重に効いて開いたままになるのを防ぐ。チップ行は
    * popoverと同じ編集UIのため操作中も閉じない。
    */
  function searchFiltersPopover(node: HTMLElement): { destroy: () => void } {
    function onPointerDown(event: PointerEvent): void {
      const target = event.target;
      if (
        target instanceof Node &&
        (node.contains(target) ||
          searchFilterToggleEl?.contains(target) === true ||
          searchFilterChipsEl?.contains(target) === true)
      ) {
        return;
      }
      onCloseSearchFilters();
    }
    function onKeydown(event: KeyboardEvent): void {
      if (event.key === "Escape") {
        event.preventDefault();
        onCloseSearchFilters();
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
</script>

<div class="search-bar" role="search" data-testid="search-bar">
  {#if !searchExpanded}
    <!-- UIUX-3 V-3: 折りたたみ1行。確定済み検索表現 + scope実態 + フィルタ/
         検索モードbadge (UIUX-7: キーワード/ハイブリッド/意味/類似) +
         実行中/表示中のクリア操作 -->
    <button
      class="search-toggle"
      data-testid="search-toggle"
      aria-expanded="false"
      aria-label="検索を展開"
      title="検索行を展開"
      onclick={onToggleSearchExpanded}
    >🔍 ▸</button>
    <span class="search-summary" data-testid="search-summary">
      {#if collapsedSearchLine.queryText !== ""}
        <span class="search-summary-query" data-testid="search-summary-query">{collapsedSearchLine.queryText}</span>
      {:else}
        <span class="search-summary-empty">検索</span>
      {/if}
    </span>
    {#if collapsedSearchLine.scopeLabel !== null}
      <span class="search-scope-badge" data-testid="search-scope-badge" data-scope-kind={searchScopeMode}>
        {collapsedSearchLine.scopeLabel}
      </span>
    {/if}
    {#each collapsedSearchLine.badges as badge (badge)}
      <span class="search-scope-badge">{badge}</span>
    {/each}
    {#if searchRunning}
      <span class="search-summary-running">検索中…</span>
    {/if}
    {#if collapsedSearchLine.showClear}
      <button
        class="search-summary-clear"
        data-testid="search-clear"
        aria-label="検索を解除"
        title="検索語を消し、検索結果表示を解除"
        onclick={onClearSearchFromCollapsed}
      >✕</button>
    {/if}
  {:else}
  <button
    class="search-toggle"
    data-testid="search-toggle"
    aria-expanded="true"
    aria-label="検索を折りたたむ"
    title="検索行を折りたたむ"
    onclick={onToggleSearchExpanded}
  >🔍 ▾</button>
  <input
    type="search"
    class="search-input"
    placeholder="検索 (自然言語OK: 夕暮れの海岸 / 構文: tag:night rating:>=3)"
    aria-label="検索クエリ"
    data-testid="search-input"
    value={searchInput}
    oninput={(event) => onSearchInput(event.currentTarget.value)}
    onkeydown={(event) => {
      if (event.key === "Enter") {
        event.preventDefault();
        void onRunSearch();
      }
    }}
  />
  <select
    class="search-scope"
    aria-label="検索範囲"
    title="検索範囲。「現在のフォルダ」は選択フォルダを含む登録ルート全体 (サブフォルダ単位の絞り込みはなし)"
    value={searchScopeMode}
    onchange={(event) =>
      onSearchScopeChange(event.currentTarget.value as typeof searchScopeMode)}
  >
    <option value="library">ライブラリ全体</option>
    <option value="folder">現在のフォルダ（登録ルート全体）</option>
    <option value="collection">表示中コレクション</option>
  </select>
  <!-- UIUX-7/A: 意味重みスライダーを3モードセグメントへ置換 (Toolbarの
       size-toggleと同一パターン)。不可用时も表示しhybrid/semanticは
       disabled — 不可の理由は各ボタンのtitleへ機能説明に連結 (modeTitle)。
       keywordは意味検索に依存せず常に使えるため常時有効 (UIUX-7/WP2)。 -->
  <span
    class="search-mode-segment"
    role="group"
    aria-label="検索モード"
    data-testid="search-mode-segment"
    title={semanticAvailable
      ? "検索モード: キーワード (0%) / ハイブリッド (50%) / 意味 (100%)"
      : semanticReason}
  >
    <button
      aria-pressed={semanticMode === "keyword"}
      class:active={semanticMode === "keyword"}
      title="構文・タグ・評価などのキーワード検索 (意味検索を使わない)"
      data-testid="search-mode-keyword"
      onclick={() => onSemanticModeChange("keyword")}
    >キーワード</button>
    <button
      aria-pressed={semanticMode === "hybrid"}
      class:active={semanticMode === "hybrid"}
      disabled={!semanticAvailable}
      title={modeTitle("キーワード検索と意味検索 (自然言語) を半々で融合")}
      data-testid="search-mode-hybrid"
      onclick={() => onSemanticModeChange("hybrid")}
    >ハイブリッド</button>
    <button
      aria-pressed={semanticMode === "semantic"}
      class:active={semanticMode === "semantic"}
      disabled={!semanticAvailable}
      title={modeTitle("自然言語の意味の近さだけで検索 (キーワード一致は使わない)")}
      data-testid="search-mode-semantic"
      onclick={() => onSemanticModeChange("semantic")}
    >意味</button>
  </span>
  <button
    bind:this={searchFilterToggleEl}
    class:active={searchFiltersOpen || searchFiltersActive}
    onclick={() => onToggleSearchFilters()}
    title="評価・選別・お気に入り・形式・タグ・日付で絞り込み (検索語とAND合成)"
    aria-expanded={searchFiltersOpen}
    aria-haspopup="true"
    data-testid="search-filter-toggle"
  >
    フィルター{searchFiltersActive ? " ●" : ""}
  </button>
  <button
    onclick={() => void onRunSearch()}
    disabled={searchRunning}
    data-testid="search-run"
  >
    {searchRunning ? "検索中…" : "検索"}
  </button>
  <button
    onclick={() => onSaveCurrentSearch()}
    disabled={!canSaveSearch}
    title="現在の検索条件を保存済み検索 (スマートコレクション) として保存"
    data-testid="search-save"
  >
    保存
  </button>
  {#if searchWarnings.length > 0}
    <span class="search-warn" title={searchWarnings.join("\n")}>
      ⚠ {searchWarnings.length}件の警告
    </span>
  {/if}
  {#if showSemanticSuggest}
    <!-- UIUX-7/C: 意味検索サジェストチップ。判定・再実行はApp — ここは
         表示とclickの受け渡しのみ (入力中は出さない)。 -->
    <button
      class="semantic-suggest-chip"
      data-testid="semantic-suggest-chip"
      aria-label="意味検索を試す"
      title="キーワードモードで実行しました。同じクエリをハイブリッド (キーワード+意味) で再検索します"
      onclick={onSemanticSuggestClick}
    >🧠 意味検索を試す</button>
  {/if}
  {#if searchFiltersOpen}
    <div class="search-filters" data-testid="search-filters" use:searchFiltersPopover>
      <label>
        評価≥
        <select
          value={searchFilters.ratingMin === null ? "" : String(searchFilters.ratingMin)}
          onchange={(event) => {
            const value = event.currentTarget.value;
            onSearchFiltersChange({
              ...searchFilters,
              ratingMin: value === "" ? null : Number(value),
            });
          }}
        >
          <option value="">-</option>
          {#each [1, 2, 3, 4, 5] as rating (rating)}
            <option value={rating}>★{rating}</option>
          {/each}
        </select>
      </label>
      <label>
        選別
        <select
          value={searchFilters.cull ?? ""}
          onchange={(event) => {
            const value = event.currentTarget.value;
            onSearchFiltersChange({
              ...searchFilters,
              cull:
                value === "picked" || value === "rejected" || value === "unselected"
                  ? value
                  : null,
            });
          }}
        >
          <option value="">-</option>
          <option value="picked">採用</option>
          <option value="rejected">除外</option>
          <option value="unselected">未選別</option>
        </select>
      </label>
      <label>
        <input
          type="checkbox"
          checked={searchFilters.favorite === true}
          onchange={(event) =>
            onSearchFiltersChange({
              ...searchFilters,
              favorite: event.currentTarget.checked ? true : null,
            })}
        />
        ♥ お気に入り
      </label>
      <label>
        形式
        <select
          value={searchFilters.type}
          onchange={(event) =>
            onSearchFiltersChange({ ...searchFilters, type: event.currentTarget.value })}
        >
          {#each ["", "png", "jpg", "webp", "gif", "avif", "bmp", "tiff"] as format (format)}
            <option value={format}>{format === "" ? "-" : format}</option>
          {/each}
        </select>
      </label>
      <label>
        タグ
        <input
          type="text"
          placeholder="tag"
          value={searchFilters.tag}
          oninput={(event) =>
            onSearchFiltersChange({ ...searchFilters, tag: event.currentTarget.value })}
        />
      </label>
      <label>
        追加日
        <input
          type="date"
          value={searchFilters.addedFrom}
          onchange={(event) =>
            onSearchFiltersChange({ ...searchFilters, addedFrom: event.currentTarget.value })}
        />
        〜
        <input
          type="date"
          value={searchFilters.addedTo}
          onchange={(event) =>
            onSearchFiltersChange({ ...searchFilters, addedTo: event.currentTarget.value })}
        />
      </label>
      <button onclick={() => onSearchFiltersChange({ ...EMPTY_FILTERS })}>クリア</button>
    </div>
  {/if}
  {#if searchFiltersActive}
    <!-- UIUX-5/WP2: 適用中 (ドラフト) フィルタのチップ行。折りたたみ時は
         展開branchごと非描画になるためここに置くだけで非表示になる -->
    <div
      class="search-filter-chips"
      bind:this={searchFilterChipsEl}
      data-testid="search-filter-chips"
    >
      {#each searchFilterChipList as chip (chip.key)}
        <span
          class="search-filter-chip"
          data-testid="search-filter-chip"
          data-filter={chip.key}
        >
          <span class="search-filter-chip-label">{chip.label}</span>
          <button
            class="search-filter-chip-remove"
            aria-label={`フィルター「${chip.label}」を解除`}
            title={`フィルター「${chip.label}」を解除`}
            onclick={() => onRemoveSearchFilterChip(chip.key)}
          >×</button>
        </span>
      {/each}
    </div>
  {/if}
  {/if}
</div>

<style>
  /* WP-8: 検索バー (UIUX-5/WP2: フィルタpopoverの配置基準) */
  .search-bar {
    position: relative;
    flex: none;
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--space-40);
    padding: 0.25rem 0.6rem;
    border-bottom: 1px solid var(--color-border-strong);
  }

  /* UIUX-3 V-3: 折りたたみトグル + 1行要約 */
  .search-toggle {
    flex: none;
    border: 1px solid var(--color-border-strong);
    background: var(--color-bg-inset);
    color: inherit;
    font: inherit;
    font-size: var(--font-size-80);
    padding: var(--space-15) var(--space-50);
    border-radius: var(--radius-sm);
    cursor: pointer;
    white-space: nowrap;
  }

  .search-summary {
    flex: 1 1 10rem;
    min-width: 0;
    display: inline-flex;
    align-items: center;
    gap: var(--space-40);
    font-size: var(--font-size-80);
    overflow: hidden;
  }

  .search-summary-query {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .search-summary-empty {
    color: var(--color-text-muted);
    font-size: var(--font-size-80);
  }

  .search-scope-badge {
    flex: none;
    display: inline-block;
    padding: 0 0.45rem;
    border: 1px solid var(--color-border-strong);
    border-radius: var(--radius-pill);
    font-size: var(--font-size-75);
    color: var(--color-text-muted);
    white-space: nowrap;
    max-width: 22rem;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .search-summary-running {
    flex: none;
    font-size: var(--font-size-75);
    color: var(--color-accent);
  }

  .search-summary-clear {
    flex: none;
    border: 1px solid var(--color-border-strong);
    background: transparent;
    color: inherit;
    font: inherit;
    padding: 0.05rem 0.45rem;
    border-radius: var(--radius-sm);
    cursor: pointer;
  }

  .search-input {
    flex: 1 1 16rem;
    min-width: 12rem;
  }

  .search-scope {
    flex: none;
  }

  /* UIUX-7/A: 検索モード3択セグメント (Toolbarのsize-toggleと同一パターン・
     同デザイントークン。選択側はbutton.active + aria-pressed)。 */
  .search-mode-segment {
    flex: none;
    display: inline-flex;
    align-items: stretch;
    font-size: var(--font-size-75);
    white-space: nowrap;
  }

  .search-mode-segment button {
    border: 1px solid var(--color-border-strong);
    border-radius: 0;
    background: transparent;
    color: inherit;
    font: inherit;
    font-size: var(--font-size-75);
    padding: var(--space-15) var(--space-50);
    cursor: pointer;
    position: relative;
  }

  .search-mode-segment button:first-of-type {
    border-radius: var(--radius-sm) 0 0 var(--radius-sm);
  }

  .search-mode-segment button:nth-of-type(2),
  .search-mode-segment button:last-of-type {
    margin-left: -1px;
  }

  .search-mode-segment button:last-of-type {
    border-radius: 0 var(--radius-sm) var(--radius-sm) 0;
  }

  /* 選択中/hoverの枠は隣接ボタンとのborder重なりに塗り潰されないよう手前に */
  .search-mode-segment button.active,
  .search-mode-segment button:hover:not(:disabled) {
    z-index: 1;
  }

  .search-mode-segment button:disabled {
    opacity: 0.55;
    cursor: default;
  }

  /* UIUX-7/C: 意味検索サジェストチップ (検索実行後のみ表示) */
  .semantic-suggest-chip {
    flex: none;
    border: 1px solid var(--color-accent);
    background: transparent;
    color: var(--color-accent);
    font: inherit;
    font-size: var(--font-size-75);
    padding: 0 0.45rem;
    border-radius: var(--radius-pill);
    cursor: pointer;
    white-space: nowrap;
  }

  .search-warn {
    font-size: var(--font-size-75);
    color: var(--color-warning);
  }

  /* UIUX-5/WP2: フィルタ群は検索バー直下のpopover (Esc/外クリックで閉じる) */
  .search-filters {
    position: absolute;
    top: 100%;
    left: var(--space-60);
    right: var(--space-60);
    z-index: 600;
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--space-60);
    padding: var(--space-60) var(--space-75);
    font-size: var(--font-size-80);
    background: var(--color-surface-raised);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-popover);
  }

  .search-filters label {
    display: inline-flex;
    align-items: center;
    gap: var(--space-30);
  }

  /* UIUX-5/WP2: 適用中 (ドラフト) フィルタのチップ行 (検索バー2行目) */
  .search-filter-chips {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--space-30);
    width: 100%;
  }

  .search-filter-chip {
    display: inline-flex;
    align-items: center;
    gap: var(--space-20);
    max-width: 22rem;
    padding: 0 0.45rem;
    border: 1px solid var(--color-border-strong);
    border-radius: var(--radius-pill);
    font-size: var(--font-size-75);
    color: var(--color-text-muted);
    white-space: nowrap;
  }

  .search-filter-chip-label {
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .search-filter-chip-remove {
    flex: none;
    border: none;
    background: transparent;
    color: inherit;
    font: inherit;
    padding: 0;
    line-height: 1;
    cursor: pointer;
  }

  /* App.svelteの全button共通則と同一値 (scoped CSSのためcomponent毎に必要)。
     class-lessボタン (検索実行・保存・フィルター・クリア) はこの則でのみ
     font-sizeが決まるため移設必須。 */
  button {
    font-size: var(--font-size-80);
  }

  button.active {
    border-color: var(--color-accent);
    font-weight: bold;
  }
</style>
