import { describe, expect, it } from "vitest";
import {
  CELL_LAYOUTS,
  GRID_GAP,
  computeColumns,
  indexToRowCol,
  rowColToIndex,
  rowExtent,
  totalRows,
} from "./layout";

describe("computeColumns", () => {
  it("幅から列数を切り下げで算出する", () => {
    // floor((1000 + 8) / (128 + 8)) = floor(7.41) = 7
    expect(computeColumns(1000, CELL_LAYOUTS.s256.cssSize)).toBe(7);
  });

  it("セルサイズを大きくすると列数が減る", () => {
    // floor((1000 + 8) / (256 + 8)) = floor(3.82) = 3
    expect(computeColumns(1000, CELL_LAYOUTS.s512.cssSize)).toBe(3);
  });

  it("端数は切り捨て (1セルも追加で入らない)", () => {
    // 128+8=136 の7倍 = 952。残り48pxにセルは入らない
    expect(computeColumns(999, CELL_LAYOUTS.s256.cssSize)).toBe(7);
  });

  it("最小1列を保証する", () => {
    expect(computeColumns(0, CELL_LAYOUTS.s256.cssSize)).toBe(1);
    expect(computeColumns(50, CELL_LAYOUTS.s256.cssSize)).toBe(1);
  });

  it("gapを含めた恰好一致", () => {
    const width = (CELL_LAYOUTS.s256.cssSize + GRID_GAP) * 8 - GRID_GAP;
    expect(computeColumns(width, CELL_LAYOUTS.s256.cssSize)).toBe(8);
  });
});

describe("totalRows", () => {
  it("列数で切り上げた行数", () => {
    expect(totalRows(50_000, 7)).toBe(Math.ceil(50_000 / 7));
  });

  it("端数は1行追加", () => {
    expect(totalRows(15, 7)).toBe(3);
    expect(totalRows(14, 7)).toBe(2);
  });

  it("0件は0行", () => {
    expect(totalRows(0, 7)).toBe(0);
  });
});

describe("rowExtent", () => {
  it("セル+gapの行高", () => {
    expect(rowExtent(CELL_LAYOUTS.s256.cssSize)).toBe(CELL_LAYOUTS.s256.cssSize + GRID_GAP);
  });
});

describe("index <-> (row, col)", () => {
  it("row, colへの分解と復元の往復", () => {
    for (const index of [0, 1, 6, 7, 8, 100, 49_999]) {
      const { row, col } = indexToRowCol(index, 7);
      expect(rowColToIndex(row, col, 7)).toBe(index);
    }
  });

  it("行末・行頭の境界", () => {
    expect(indexToRowCol(6, 7)).toEqual({ row: 0, col: 6 });
    expect(indexToRowCol(7, 7)).toEqual({ row: 1, col: 0 });
  });

  it("負の列は前行の末尾として計算される (Left折返し計算用)", () => {
    // row1 col0 の1つ前 = row0 の末尾
    expect(rowColToIndex(1, -1, 7)).toBe(6);
    expect(rowColToIndex(3, -1, 7)).toBe(20);
  });
});
