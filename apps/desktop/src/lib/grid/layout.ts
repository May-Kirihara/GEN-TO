/**
 * グリッドレイアウトの純粋計算 (WP-5 / 本体計画 §10)。
 *
 * 一定セルサイズのグリッド: 列数はビューポート幅から切り下げ、
 * 行数は total/columns の切り上げ。行単位で仮想化する。
 */

export type ThumbCellClass = "s256" | "s512";

/** セルサイズとサムネイルサイズクラスの対応 (HiDPI考慮で実表示の2倍クラス)。 */
export const CELL_LAYOUTS: Record<ThumbCellClass, { cssSize: number; label: string }> = {
  s256: { cssSize: 128, label: "小 (128px)" },
  s512: { cssSize: 256, label: "大 (256px)" },
};

/** セル間・行間の余白 (px)。 */
export const GRID_GAP = 8;

/** 列数: max(1, floor((width + gap) / (cell + gap)))。 */
export function computeColumns(viewportWidth: number, cellSize: number, gap = GRID_GAP): number {
  if (cellSize <= 0) {
    return 1;
  }
  return Math.max(1, Math.floor((viewportWidth + gap) / (cellSize + gap)));
}

/** 行数: ceil(total / columns)。 */
export function totalRows(total: number, columns: number): number {
  if (total <= 0 || columns <= 0) {
    return 0;
  }
  return Math.ceil(total / columns);
}

/** 行の高さ (セル+gap)。estimateSizeに渡す。 */
export function rowExtent(cellSize: number, gap = GRID_GAP): number {
  return cellSize + gap;
}

/** index → (row, col)。 */
export function indexToRowCol(index: number, columns: number): { row: number; col: number } {
  return { row: Math.floor(index / columns), col: index % columns };
}

/** (row, col) → index。負のcolは前行の末尾へ正規化 (Left折返し計算)。 */
export function rowColToIndex(row: number, col: number, columns: number): number {
  return row * columns + col;
}
