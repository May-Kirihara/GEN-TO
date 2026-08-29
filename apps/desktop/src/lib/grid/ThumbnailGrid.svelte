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
  import type { GridStore } from "./grid-store";
  import type { ScrollPositionStore } from "./scroll-position";
  import type { ImageListItem } from "../catalog-types";

  let {
    grid,
    scrollStore,
    modeKey,
    cellClass,
    overscanRows,
  }: {
    grid: GridStore;
    scrollStore: ScrollPositionStore;
    /** スクロール位置保存キー (モード別)。 */
    modeKey: string;
    cellClass: ThumbCellClass;
    overscanRows: number;
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
  // 再起動直後はin-memoryが空なので先頭から (復元は開発パネルのボタン)。
  const restoredOffset = scrollStore.peek(modeKey) ?? 0;

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
      scrollStore.saveThrottled(modeKey, offset);
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
    followFocus();
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

  function cellsOf(rowIndex: number): Array<{ index: number; key: string }> {
    void gridTick;
    const cells: Array<{ index: number; key: string }> = [];
    for (let col = 0; col < columns; col++) {
      const index = rowIndex * columns + col;
      if (index >= total) {
        break;
      }
      const item = grid.getItem(index);
      cells.push({ index, key: item ? item.id : `pending-${index}` });
    }
    return cells;
  }

  function statsLine(): string {
    void gridTick;
    const stats = grid.stats;
    return (
      `要求 ${stats.requested.toLocaleString()} / 拒否 ${stats.rejectedFull.toLocaleString()}` +
      ` / ready受信 ${stats.readyReceived.toLocaleString()} / 失敗 ${stats.failedReceived.toLocaleString()}`
    );
  }

  /** 開発パネルの「位置復元」: localStorageの値へ明示的に戻す。 */
  export function restoreFromStorage(): void {
    const top = scrollStore.load(modeKey);
    if (top !== null) {
      virtualizerInstance?.scrollToOffset(top, { align: "start" });
    }
  }

  onDestroy(() => {
    stopGridNotify();
    scrollStore.flush();
  });
</script>

<div class="grid-stats">
  <span>DOMセル数 <strong>{domCells}</strong></span>
  <span>可視row {visRange === null ? "—" : `${visRange.start}..${visRange.end}`}</span>
  <span>overscan {overscanRows}</span>
  <span>列 {columns}</span>
  <span>total {total.toLocaleString()}</span>
  <span>scrollTop {Math.round(scrollTop)}</span>
  <span>選択 {sel.selected.size.toLocaleString()}</span>
  <span>需要 {statsLine()}</span>
</div>

<div
  bind:this={scrollEl}
  class="grid-scroll"
  role="grid"
  aria-label="サムネイルグリッド"
  tabindex="0"
  onkeydown={handleKeydown}
  onclick={handleClick}
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
          <div
            role="gridcell"
            class="cell"
            class:cell-selected={item !== null && sel.selected.has(item.id)}
            class:cell-focus={sel.focusIndex === cell.index}
            data-grid-cell
            data-index={cell.index}
            style="width:{cellPx}px; height:{cellPx}px"
            aria-selected={item !== null && sel.selected.has(item.id)}
            title={item?.displayName ?? ""}
          >
            {#if item !== null}
              {@const url = urlFor(item)}
              {#if url !== null}
                <img src={url} alt={item.displayName} loading="lazy" decoding="async" draggable="false" />
              {/if}
            {/if}
          </div>
        {/each}
      </div>
    {/each}
  </div>
</div>

<style>
  .grid-stats {
    display: flex;
    flex-wrap: wrap;
    gap: 0.75rem;
    font-size: 0.75rem;
    font-family: monospace;
    padding: 0.25rem 0;
  }

  .grid-scroll {
    height: 70vh;
    overflow-y: auto;
    border: 1px solid light-dark(#d0d7de, #30363d);
    border-radius: 0.5rem;
    outline: none;
  }

  .grid-scroll:focus-visible {
    border-color: light-dark(#0969da, #58a6ff);
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
    border-radius: 0.25rem;
    background: light-dark(#eaeef2, #21262d);
    border: 1px solid light-dark(#d0d7de, #30363d);
    overflow: hidden;
    box-sizing: border-box;
  }

  .cell:focus {
    outline: none;
  }

  .cell-selected {
    border-color: light-dark(#0969da, #58a6ff);
    box-shadow: 0 0 0 2px light-dark(#0969da, #58a6ff);
  }

  .cell-focus {
    border-color: light-dark(#bf8700, #d29922);
  }

  .cell img {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    object-fit: contain;
    pointer-events: none;
  }
</style>
