<script lang="ts">
  /**
    * 検索行 (UIUX-5/WP7bでApp.svelteから抽出)。折りたたみ1行・展開行・
    * フィルタpopover・チップ行を含む (search-barのposition:relativeが
    * popoverの配置基準のため同一component内に置く)。
    * 状態・storeはApp残置 — 素値をpropsで受け取り操作はcallbackで返す。
    * searchFiltersPopover (閉じaction) は要素と同じcomponent内にあるため
    * ここへ移設 (window listenerのためscoped制約なし — WP7a契約踏襲)。
    */
  import { EMPTY_FILTERS, weightLabel, type GuiFilters } from "../search";
  import type { CollapsedSearchSummary } from "../ui/search-summary";
  import type { FilterChip, FilterChipKey } from "../ui/filter-chips";

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
    semanticWeightPercent,
    onSemanticWeightInput,
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
    semanticWeightPercent: number;
    onSemanticWeightInput: (value: number) => void;
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
         意味重み + 実行中/表示中のクリア操作 -->
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
    placeholder="検索 (例: 青い髪の少女 tag:night rating:>=3 type:png)"
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
  <label
    class="search-semantic"
    data-testid="search-semantic"
    title={semanticAvailable
      ? "キーワード検索と意味検索 (自然言語) の重み。左=キーワードのみ / 右=意味のみ"
      : semanticReason}
  >
    <span class="semantic-end">キーワード</span>
    <input
      type="range"
      min="0"
      max="100"
      step="5"
      value={semanticWeightPercent}
      disabled={!semanticAvailable}
      aria-label="意味検索の重み"
      data-testid="search-semantic-weight"
      oninput={(event) => onSemanticWeightInput(Number(event.currentTarget.value))}
    />
    <span class="semantic-end">意味</span>
    <span class="semantic-value" data-testid="search-semantic-value">
      {semanticWeightPercent > 0
        ? `${weightLabel(semanticWeightPercent / 100)} ${semanticWeightPercent}%`
        : "キーワード"}
    </span>
  </label>
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

  /* WP-12: キーワード⇔意味の重みスライダー */
  .search-semantic {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: var(--space-35);
    font-size: var(--font-size-75);
    white-space: nowrap;
  }

  .search-semantic input[type="range"] {
    width: 7rem;
  }

  .search-semantic:has(input:disabled) {
    opacity: 0.55;
  }

  .semantic-value {
    min-width: 6.5rem;
    font-variant-numeric: tabular-nums;
    text-align: left;
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
