/**
 * UIUX-3 V-2: グリッド統計のスナップショット型。
 *
 * ThumbnailGridから主UIのオーバーレイ表示を廃止し (UIUX-3)、統計の**計算は
 * ThumbnailGridに残したまま** `getStatsSnapshot()` (同期・純読取) で外部へ
 * 提供する。消費側 (DevDrawer) はパネルを開いている間だけpollする —
 * 毎フレームのApp $stateへのliftは再描画嵐になるため禁止 (計画書「改訂」§2)。
 * E2E (devtest G行) が使う `data-grid-cell` / `.grid-scroll` のDOMフックは
 * 統計表示とは独立に温存される。
 */
export type GridStatsSnapshot = {
  /** 実DOMに存在するセル数 (rAF後の観測値)。 */
  domCells: number;
  /** 可視行範囲 (未測定=null)。 */
  visStart: number | null;
  visEnd: number | null;
  overscanRows: number;
  columns: number;
  total: number;
  scrollTop: number;
  selectionCount: number;
  /** サムネイル需要統計 (GridStore.stats)。 */
  demand: {
    requested: number;
    rejectedFull: number;
    cancelled: number;
    readyReceived: number;
    failedReceived: number;
  };
};
