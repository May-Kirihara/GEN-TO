<script lang="ts">
  /**
   * 仮想サムネイルグリッド (WP-5 / フェーズ3)。
   *
   * - @tanstack/svelte-virtual で行単位の仮想化 (各行はcolumns個のセル)
   * - セルサイズ・overscanはAppからpropsで受ける。セルクラス変更は
   *   親の {#key} でインスタンスを作り直す (estimateSize変更は測定キャッシュに
   *   効かないため。データはGridStore側に残るので再取得不要)
   * - 選択はselection.tsの純粋関数 + スナップショット再代入
   *   (セルは selected boolean を個別に評価。全項目への埋め込みはしない)
   * - キーボード移動は scrollIntoView 相当 (virtualizer.scrollToIndex)
   */
  import { onDestroy } from "svelte";
  import {
    createVirtualizer,
    type SvelteVirtualizer,
    type VirtualItem,
  } from "@tanstack/svelte-virtual";
  import {
    CELL_LAYOUTS,
    GRID_GAP,
    computeColumns,
    indexToRowCol,
    rowExtent,
    totalRows,
    type ThumbCellClass,
  } from "./layout";
  import {
    applyKey,
    applyPointer,
    emptySelection,
    type MoveAction,
  } from "./selection";
  import { applyWheelScroll, conditionalWheel } from "./wheel-speed";
  import type { GridStatsSnapshot } from "./grid-stats";
  import type { GridStore } from "./grid-store";
  import type { ScrollPositionStore } from "./scroll-position";
  import type { ImageListItem } from "../catalog-types";

  let {
    grid,
    scrollStore,
    modeKey,
    cellClass,
    overscanRows,
    scrollSpeed = 1,
    onOpen,
    onSelectionChange,
    onContextMenu,
    renameId = null,
    renameInitial = "",
    onRenameCommit,
    onRenameCancel,
    dragPathsOf,
    tooltipOf = null,
  }: {
    grid: GridStore;
    scrollStore: ScrollPositionStore;
    /** スクロール位置保存キー (モード・フォルダ別)。 */
    modeKey: string;
    cellClass: ThumbCellClass;
    overscanRows: number;
    /** UIUX-1: ホイールスクロール速度係数 (既定1.0=ネイティブ。1.0ではリスナ未登録)。 */
    scrollSpeed?: number;
    /** ダブルクリック/中央キー(Enter)でビューアを開く (WP-6)。indexを渡す。 */
    onOpen?: (index: number) => void;
    /** 選択変化の通知 (比較表示の充填ソース用、WP-6)。 */
    onSelectionChange?: (snapshot: { ids: string[]; focusIndex: number | null }) => void;
    /** セルのコンテキストメニュー要求 (WP-4)。 */
    onContextMenu?: (event: MouseEvent, index: number) => void;
    /** WP-7: リネーム中の項目id (F2インライン編集)。 */
    renameId?: string | null;
    /** WP-7: リネーム編集の初期値 (元の表示名)。 */
    renameInitial?: string;
    /** WP-7: インラインリネームの確定 (Enter)。 */
    onRenameCommit?: (name: string) => void;
    /** WP-7: インラインリネームの中止 (Escape/フォーカス喪失)。 */
    onRenameCancel?: () => void;
    /** WP-7: ドラッグで運ぶパス列 (選択全体。未指定ならD&D無効)。 */
    dragPathsOf?: (index: number) => string[];
    /** WP-8: セルツールチップ文言の構築 (検索一致根拠の併記。既定=表示名)。 */
    tooltipOf?: ((item: ImageListItem) => string) | null;
  } = $props();

  // GridStoreはプレーンクラスのため、版番号でUIへ変更を伝える (WP-2の教訓)
  let gridTick = $state(0);
  const stopGridNotify = grid.onChange(() => {
    gridTick++;
  });

  const cellPx = $derived(CELL_LAYOUTS[cellClass].cssSize);
  const extent = $derived(rowExtent(cellPx));

  let scrollEl: HTMLElement | null = $state(null);
  let viewportWidth = $state(0);
  const columns = $derived(computeColumns(viewportWidth, cellPx));

  function currentTotal(): number {
    void gridTick;
    return grid.summary?.total ?? 0;
  }

  const total = $derived(currentTotal());
  const rowCount = $derived(totalRows(total, columns));

  let vitems: Array<VirtualItem> = $state([]);
  let totalSize = $state(0);
  let visRange: { start: number; end: number } | null = $state(null);
  let scrollTop = $state(0);
  let domCells = $state(0);

  // モード切替・セルサイズ変更でコンポーネントを作り直す際の初期位置。
  // in-memory優先、無ければlocalStorage (再起動後の自動復元、WP-4)。
  // (関数内で読むことで「初期値のみ参照」の意図を明示する)
  function computeRestoredOffset(): number {
    const inMemory = scrollStore.peek(modeKey);
    if (inMemory !== null) {
      return inMemory;
    }
    return scrollStore.load(modeKey) ?? 0;
  }
  const restoredOffset = computeRestoredOffset();
  // 起動直後 (フォルダ切替直後) はカタログが空でscrollTopが保持できないため、
  // 内容が載った時点 (total>0) で改めて復元する
  let pendingRestore: number | null = restoredOffset > 0 ? restoredOffset : null;
  // 再走査 (同一フォルダのbegin_scan) でrecordsが破棄されるとscrollTopが
  // 0へ落ちる。その直前の位置を覚えておき、内容が戻った時点で復元する
  // (QA-WP4 Minor-6。{#key}による再構築が走らない同一フォルダ再走査向け)。
  let lastPositiveOffset = restoredOffset;

  const virtualizer = createVirtualizer({
    count: rowCount,
    getScrollElement: () => scrollEl,
    estimateSize: () => extent,
    overscan: overscanRows,
    initialOffset: restoredOffset,
    // ResizeObserverの同一フレーム再通知によるloop警告の抑制 (既定のrAF遅延)
    useAnimationFrameWithResizeObserver: true,
  });

  // 派生ストアの値 (Virtualizerインスタンス) を購読時に捕捉する。
  // Readable自体にはsetOptions等のメソッドがないため。
  let virtualizerInstance: SvelteVirtualizer<HTMLElement, Element> | null = null;

  function applySnapshot(instance: SvelteVirtualizer<HTMLElement, Element>): void {
    vitems = instance.getVirtualItems().map((item) => ({ ...item }));
    totalSize = instance.getTotalSize();
    const range = instance.range;
    visRange = range === null ? null : { start: range.startIndex, end: range.endIndex };
    const offset = instance.scrollOffset ?? 0;
    if (offset !== scrollTop) {
      scrollTop = offset;
      // 空カタログ中のscrollTop=0は「実位置」ではないため保存しない
      // (再走査で保存位置が0で上書きされるのを防ぐ — QA-WP4 Minor-6)
      if (offset > 0 || total > 0) {
        scrollStore.saveThrottled(modeKey, offset);
      }
      if (offset > 0) {
        lastPositiveOffset = offset;
      }
    }
  }

  // 購読開始でvirtualizerのobserveElementRect/Offsetが繋がる。
  // 購読のstart関数がinitialOptionsを再適用するため、続けて現在値を再設定する。
  $effect(() => {
    const unsubscribe = virtualizer.subscribe((instance) => {
      virtualizerInstance = instance;
      applySnapshot(instance);
    });
    syncOptions();
    return unsubscribe;
  });

  function syncOptions(): void {
    virtualizerInstance?.setOptions({
      count: rowCount,
      overscan: overscanRows,
      estimateSize: () => extent,
    });
  }

  // count (summary/列数変化) とoverscanの追従。estimateSizeの変更は
  // 測定キャッシュに反映されないためセルクラス変更では再生成する。
  $effect(() => {
    syncOptions();
  });

  // カタログが空の間はscrollTopを保持できないため、total>0になった時点で
  // 復元位置へjumpする (起動・フォルダ切替直後の復元、WP-4)。
  // 同一フォルダ再走査 (totalがN→0→Nへ遷移) では、直前の非ゼロ位置へ
  // 戻す (QA-WP4 Minor-6)。
  $effect(() => {
    void total;
    if (virtualizerInstance === null) {
      return;
    }
    if (total === 0) {
      if (pendingRestore === null && lastPositiveOffset > 0) {
        pendingRestore = lastPositiveOffset;
      }
      return;
    }
    if (pendingRestore !== null) {
      const target = pendingRestore;
      pendingRestore = null;
      virtualizerInstance.scrollToOffset(target, { align: "start" });
    }
  });

  // 幅は自前のResizeObserverで1回だけ反映 (コンテンツはスクロール要素の
  // サイズに影響しない絶対配置のため更新ループは発生しない)
  $effect(() => {
    const element = scrollEl;
    if (element === null || typeof ResizeObserver === "undefined") {
      return;
    }
    const observer = new ResizeObserver((entries) => {
      const width = Math.round(entries[0]?.contentRect.width ?? 0);
      if (Math.abs(width - viewportWidth) >= 1) {
        viewportWidth = width;
      }
    });
    observer.observe(element);
    return () => {
      observer.disconnect();
    };
  });

  // 可視範囲→ページ取得・需要計画 (GridStore内でデバウンス)
  $effect(() => {
    grid.viewportChanged(visRange, overscanRows, scrollTop, columns);
  });

  // 実レンダー数のカウント (rAF後)
  $effect(() => {
    void vitems;
    void gridTick;
    const frame = requestAnimationFrame(() => {
      domCells = scrollEl?.querySelectorAll("[data-grid-cell]").length ?? 0;
    });
    return () => {
      cancelAnimationFrame(frame);
    };
  });

  // ===== 選択 =====

  let sel = $state(emptySelection());

  function idAt(index: number): string | null {
    return grid.getItem(index)?.id ?? null;
  }

  function pageRows(): number {
    const height = scrollEl?.clientHeight ?? 0;
    return Math.max(1, Math.floor(height / Math.max(1, extent)));
  }

  const KEY_ACTIONS: Record<string, MoveAction> = {
    ArrowLeft: "ArrowLeft",
    ArrowRight: "ArrowRight",
    ArrowUp: "ArrowUp",
    ArrowDown: "ArrowDown",
    Home: "Home",
    End: "End",
    PageUp: "PageUp",
    PageDown: "PageDown",
  };

  function handleKeydown(event: KeyboardEvent): void {
    // Enter (ビューア) とSpace (クイックプレビュー) はAppのKeyDispatcherが
    // captureで先に処理する (WP-4の一元化)。ここでは選択移動のみ。
    const action = KEY_ACTIONS[event.key];
    if (!action) {
      return;
    }
    event.preventDefault();
    sel = applyKey(
      sel,
      action,
      { shift: event.shiftKey, total, columns, pageRows: pageRows() },
      idAt,
    );
    notifySelection();
    followFocus();
  }

  function notifySelection(): void {
    onSelectionChange?.({ ids: [...sel.selected], focusIndex: sel.focusIndex });
  }

  function followFocus(): void {
    const focus = sel.focusIndex;
    if (focus === null) {
      return;
    }
    const { row } = indexToRowCol(focus, columns);
    if (visRange !== null && row >= visRange.start && row <= visRange.end) {
      return;
    }
    virtualizerInstance?.scrollToIndex(row, { align: "auto" });
  }

  function handleClick(event: MouseEvent): void {
    const cell = (event.target as HTMLElement).closest<HTMLElement>("[data-grid-cell]");
    if (cell === null) {
      return;
    }
    const index = Number(cell.dataset.index);
    if (!Number.isInteger(index) || index < 0 || index >= total) {
      return;
    }
    sel = applyPointer(
      sel,
      index,
      { ctrl: event.ctrlKey || event.metaKey, shift: event.shiftKey },
      total,
      idAt,
    );
    notifySelection();
  }

  function handleDblClick(event: MouseEvent): void {
    const cell = (event.target as HTMLElement).closest<HTMLElement>("[data-grid-cell]");
    if (cell === null) {
      return;
    }
    const index = Number(cell.dataset.index);
    if (Number.isInteger(index) && index >= 0 && index < total) {
      onOpen?.(index);
    }
  }

  function handleContextMenu(event: MouseEvent): void {
    if (onContextMenu === undefined) {
      return;
    }
    const cell = (event.target as HTMLElement).closest<HTMLElement>("[data-grid-cell]");
    if (cell === null) {
      return;
    }
    const index = Number(cell.dataset.index);
    if (Number.isInteger(index) && index >= 0 && index < total) {
      event.preventDefault();
      onContextMenu(event, index);
    }
  }

  // ===== UIUX-1: ホイールスクロール速度 (R-C) =====

  /**
    * 係数≠1.0のときだけ速度を適用するwheelハンドラ (action経由でpassive:false)。
    * 適用パイプライン (正規化→ctrlKey素通り→境界判定→scrollBy) は
    * wheel-speed.applyWheelScroll — ctrlKey受け渡しまで単体テストで保証
    * するため分離 (r2)。端の外向きdeltaではpreventDefaultしない = 親
    * スクロールへのチェイニングを阻害しない (r1 B1)。
    */
  function handleWheel(event: WheelEvent): void {
    applyWheelScroll(event, scrollSpeed);
  }

  // ===== WP-7: リネーム (F2インライン) =====

  let renameValue = $state("");
  // 編集開始 (renameId変化) のたびに初期値へリセットする
  $effect(() => {
    const current = renameId;
    if (current !== null) {
      renameValue = renameInitial;
    }
  });

  function isRenaming(item: ImageListItem | null): boolean {
    return item !== null && renameId !== null && item.id === renameId;
  }

  function submitRename(): void {
    const value = renameValue.trim();
    renameValue = "";
    if (value !== "") {
      onRenameCommit?.(value);
    } else {
      onRenameCancel?.();
    }
  }

  function cancelRename(): void {
    renameValue = "";
    onRenameCancel?.();
  }

  // ===== WP-7: ドラッグ (グリッド→フォルダツリー) =====

  function handleDragStart(event: DragEvent, index: number): void {
    if (dragPathsOf === undefined) {
      return;
    }
    const paths = dragPathsOf(index);
    if (paths.length === 0) {
      return;
    }
    if (event.dataTransfer !== null) {
      event.dataTransfer.setData("application/x-gento-paths", JSON.stringify(paths));
      event.dataTransfer.effectAllowed = "copyMove";
    }
  }

  /** リネーム入力をDOM挿入時にフォーカスする (Svelte action)。 */
  function autofocusAction(node: HTMLInputElement): void {
    node.focus();
    node.select();
  }

  /** プログラム選択 (セッション位置・クイックプレビュー移動の同期、WP-4)。 */
  export function selectAndFocus(index: number): void {
    if (!Number.isInteger(index) || index < 0 || index >= total) {
      return;
    }
    sel = applyPointer(sel, index, { ctrl: false, shift: false }, total, idAt);
    notifySelection();
    followFocus();
  }

  // ===== テンプレートヘルパー (gridTickを読み変化を伝える) =====

  function itemAt(index: number): ImageListItem | null {
    void gridTick;
    return grid.getItem(index);
  }

  function urlFor(item: ImageListItem): string | null {
    void gridTick;
    return grid.thumbUrlFor(item.id);
  }

  /** このセルが現在のサイズクラスで生成失敗しているか (WP-7 A2)。 */
  function failed(item: ImageListItem): boolean {
    void gridTick;
    return grid.isFailed(item.id);
  }

  /** 失敗セルの説明 (理由付きtooltip。WP-17 H-1)。 */
  function failReasonTitle(item: ImageListItem): string {
    void gridTick;
    const reason = grid.failureReasonFor(item.id);
    return reason === null ? "サムネイル生成失敗" : `サムネイル生成失敗 — ${reason}`;
  }

  /**
   * セルバッジ表示値 (WP-5)。pagerのin-placeパッチ (asset:updated) は項目
   * オブジェクトの同一性を保つため、gridTickを読むへルパ経由で評価する
   * (urlForと同じ規約 — 参照が同じままだとSvelteの細粒度更新が走らない)。
   */
  function cellBadges(
    item: ImageListItem,
  ): { rating: number; favorite: boolean; triage: "undecided" | "keep" | "reject" } | null {
    void gridTick;
    if (item.rating <= 0 && !item.favorite && item.triage === "undecided") {
      return null;
    }
    return { rating: item.rating, favorite: item.favorite, triage: item.triage };
  }

  function cellsOf(rowIndex: number): Array<{ index: number; key: string }> {
    void gridTick;
    const cells: Array<{ index: number; key: string }> = [];
    for (let col = 0; col < columns; col++) {
      const index = rowIndex * columns + col;
      if (index >= total) {
        break;
      }
      const item = grid.getItem(index);
      // キーは位置とidの合成: libraryスナップショットはBLAKE3重複ファイルが
      // 同一Assetを共有するため item.id 単独では行内で重複し得る
      // (1615ファイル→974アセット等)。keyed eachのキー重複はSvelte 5が
      // each_key_duplicateでレンダリング効果を落とし、以後その行が
      // 永久に更新されなくなる (FIXLIB-1)。
      cells.push({ index, key: item ? `${item.id}@${index}` : `pending-${index}` });
    }
    return cells;
  }

  /**
    * UIUX-3 V-2: グリッド統計のスナップショット (同期・純読取)。
    * 主UIのオーバーレイ表示は廃止 — DevDrawerが開いている間だけpollして
    * 表示する。gridTickを読むことで需要統計 (GridStore.stats) の変化も
    * 最新値として返す (旧オーバーレイのstatsLineと同じ規約)。
    */
  export function getStatsSnapshot(): GridStatsSnapshot {
    void gridTick;
    const stats = grid.stats;
    return {
      domCells,
      visStart: visRange === null ? null : visRange.start,
      visEnd: visRange === null ? null : visRange.end,
      overscanRows,
      columns,
      total,
      scrollTop: Math.round(scrollTop),
      selectionCount: sel.selected.size,
      demand: {
        requested: stats.requested,
        rejectedFull: stats.rejectedFull,
        cancelled: stats.cancelled,
        readyReceived: stats.readyReceived,
        failedReceived: stats.failedReceived,
      },
    };
  }

  /** 開発パネルの「位置復元」: localStorageの値へ明示的に戻す。 */
  export function restoreFromStorage(): void {
    const top = scrollStore.load(modeKey);
    if (top !== null) {
      virtualizerInstance?.scrollToOffset(top, { align: "start" });
    }
  }

  /** スクロール要素へフォーカスする (計測ブリッジのキー入力用、WP-7)。 */
  export function focus(): void {
    scrollEl?.focus();
  }

  /** スクロール高さの割合 (0=先頭, 1=末尾) へジャンプする (§16 シナリオ9計測用)。 */
  export function jumpTo(fraction: number): void {
    const el = scrollEl;
    if (el === null) {
      return;
    }
    const clamped = Math.min(1, Math.max(0, fraction));
    el.scrollTop = Math.round((el.scrollHeight - el.clientHeight) * clamped);
  }

  onDestroy(() => {
    stopGridNotify();
    scrollStore.flush();
  });
</script>

<div class="grid-root">
  <div
    bind:this={scrollEl}
    class="grid-scroll"
    role="grid"
    aria-label="サムネイルグリッド"
    tabindex="0"
    onkeydown={handleKeydown}
    onclick={handleClick}
    ondblclick={handleDblClick}
    oncontextmenu={handleContextMenu}
    use:conditionalWheel={{ enabled: scrollSpeed !== 1, handler: handleWheel }}
  >
    <div class="grid-inner" style="height:{totalSize}px">
      {#each vitems as row (row.key)}
        <div
          role="row"
          class="grid-row"
          style="position:absolute; top:0; left:0; width:100%; height:{row.size}px; transform:translateY({row.start}px); display:grid; grid-template-columns:repeat({columns}, {cellPx}px); gap:{GRID_GAP}px; justify-content:center"
        >
          {#each cellsOf(row.index) as cell (cell.key)}
            {@const item = itemAt(cell.index)}
            {@const badges = item !== null ? cellBadges(item) : null}
            <div
              role="gridcell"
              class="cell"
              class:cell-selected={item !== null && sel.selected.has(item.id)}
              class:cell-focus={sel.focusIndex === cell.index}
              class:cell-failed={item !== null && failed(item)}
              data-grid-cell
              data-index={cell.index}
              style="width:{cellPx}px; height:{cellPx}px"
              aria-selected={item !== null && sel.selected.has(item.id)}
              title={item !== null ? (tooltipOf?.(item) ?? item.displayName) : ""}
              draggable={dragPathsOf !== undefined && !isRenaming(item)}
              tabindex={-1}
              ondragstart={(event) => handleDragStart(event, cell.index)}
            >
              {#if item !== null}
                {@const url = urlFor(item)}
                {#if url !== null}
                  <img src={url} alt={item.displayName} loading="lazy" decoding="async" draggable="false" />
                {:else if failed(item)}
                  <span class="cell-fail-mark" role="img" aria-label="サムネイル生成失敗" title={failReasonTitle(item)}>✕</span>
                {/if}
                {#if badges !== null}
                  <span class="cell-badges" aria-hidden="true">
                    {#if badges.favorite}<span class="badge badge-fav" title="お気に入り">♥</span>{/if}
                    {#if badges.rating > 0}<span class="badge badge-rating" title="評価">★{badges.rating}</span>{/if}
                    {#if badges.triage === "keep"}<span class="badge badge-keep" title="採用">✓</span>{/if}
                    {#if badges.triage === "reject"}<span class="badge badge-reject" title="除外">✗</span>{/if}
                  </span>
                {/if}
                {#if isRenaming(item)}
                  <input
                    class="rename-input"
                    data-testid="rename-input"
                    use:autofocusAction
                    value={renameValue}
                    aria-label="新しい名前"
                    onclick={(event) => event.stopPropagation()}
                    oninput={(event) => (renameValue = event.currentTarget.value)}
                    onkeydown={(event) => {
                      // Enter/Escapeのみ処理して伝播は止めない (入力中の
                      // 他キーは編集対象として素通し — dev bridgeのF-keyも
                      // ここを通ってwindowへ到達する必要があるため)。
                      if (event.key === "Enter") {
                        event.preventDefault();
                        submitRename();
                      } else if (event.key === "Escape") {
                        event.preventDefault();
                        cancelRename();
                      }
                    }}
                    onblur={() => cancelRename()}
                  />
                {/if}
              {/if}
            </div>
          {/each}
        </div>
      {/each}
    </div>
  </div>
</div>

<style>
  .grid-root {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
  }

  .grid-scroll {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    border: 1px solid var(--color-border-strong);
    border-radius: var(--radius-lg);
    outline: none;
    box-sizing: border-box;
  }

  .grid-scroll:focus-visible {
    border-color: var(--color-accent);
  }

  .grid-inner {
    position: relative;
    width: 100%;
  }

  .grid-row {
    box-sizing: border-box;
  }

  .cell {
    position: relative;
    border-radius: var(--radius-xs);
    background: var(--color-bg-subtle);
    border: 1px solid var(--color-border-strong);
    overflow: hidden;
    box-sizing: border-box;
  }

  .cell:focus {
    outline: none;
  }

  .cell-selected {
    border-color: var(--color-accent);
    box-shadow: 0 0 0 2px var(--color-accent);
  }

  /* 生成失敗セル (WP-7 A2): プレースホルダーと区別される失敗表示 */
  .cell-failed {
    border-color: var(--color-danger);
    background: light-dark(#fdf0f1, #2d1517);
  }

  .cell-fail-mark {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
    color: var(--color-danger);
    font-weight: bold;
    font-size: 1.25rem;
    user-select: none;
  }

  .cell-focus {
    border-color: var(--color-attention-border);
  }

  .cell img {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    object-fit: contain;
    pointer-events: none;
  }

  /* WP-5: 評価・選別・お気に入りバッジ (左下に重ねる) */
  .cell-badges {
    position: absolute;
    left: 2px;
    bottom: 2px;
    z-index: 1;
    display: flex;
    gap: 2px;
    pointer-events: none;
  }

  .badge {
    display: inline-flex;
    align-items: center;
    padding: 0 4px;
    border-radius: 4px;
    font-size: var(--font-size-70);
    line-height: 1.3;
    color: light-dark(#ffffff, #f0f6fc);
    background: light-dark(#000000aa, #000000aa);
    user-select: none;
  }

  .badge-rating {
    color: light-dark(#eac54f, #e3b341);
  }

  .badge-fav {
    color: light-dark(#ff6b81, #f85149);
  }

  .badge-keep {
    background: light-dark(#1a7f37dd, #238636dd);
  }

  .badge-reject {
    background: light-dark(#cf222edd, #b62324dd);
  }

  /* WP-7: F2インラインリネーム入力 (セル下端に重ねる) */
  .rename-input {
    position: absolute;
    left: 2px;
    right: 2px;
    bottom: 2px;
    z-index: 2;
    font-size: var(--font-size-75);
    padding: 0.1rem 0.25rem;
    border: 1px solid var(--color-accent);
    border-radius: var(--radius-xs);
    background: light-dark(#ffffff, #0d1117);
    color: inherit;
    min-width: 0;
  }
</style>
