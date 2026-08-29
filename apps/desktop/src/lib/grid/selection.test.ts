import { describe, expect, it } from "vitest";
import {
  applyKey,
  applyPointer,
  emptySelection,
  moveIndexFrom,
  type SelectionState,
} from "./selection";

const TOTAL = 20;
const COLUMNS = 5;

function idAt(index: number): string | null {
  return `id-${index}`;
}

function ids(state: SelectionState): string[] {
  return [...state.selected].sort();
}

describe("applyPointer — クリック選択", () => {
  it("単一クリックは選択を1件に置き換え、アンカーとフォーカスを更新する", () => {
    const state = applyPointer(emptySelection(), 3, {}, TOTAL, idAt);
    expect(ids(state)).toEqual(["id-3"]);
    expect(state.anchorIndex).toBe(3);
    expect(state.focusIndex).toBe(3);
  });

  it("Ctrl+クリックは選択をトグルする (追加)", () => {
    let state = applyPointer(emptySelection(), 3, {}, TOTAL, idAt);
    state = applyPointer(state, 7, { ctrl: true }, TOTAL, idAt);
    expect(ids(state)).toEqual(["id-3", "id-7"]);
    expect(state.anchorIndex).toBe(7);
  });

  it("Ctrl+クリックは選択済み項目を除去する", () => {
    let state = applyPointer(emptySelection(), 3, {}, TOTAL, idAt);
    state = applyPointer(state, 7, { ctrl: true }, TOTAL, idAt);
    state = applyPointer(state, 3, { ctrl: true }, TOTAL, idAt);
    expect(ids(state)).toEqual(["id-7"]);
    expect(state.anchorIndex).toBe(3);
  });

  it("Shift+クリックはアンカーからの範囲で選択を置き換える", () => {
    let state = applyPointer(emptySelection(), 3, {}, TOTAL, idAt);
    state = applyPointer(state, 7, { shift: true }, TOTAL, idAt);
    expect(ids(state)).toEqual(["id-3", "id-4", "id-5", "id-6", "id-7"]);
    // アンカーは移動しない
    expect(state.anchorIndex).toBe(3);
    expect(state.focusIndex).toBe(7);
  });

  it("Shift+クリックは逆方向の範囲も選択する", () => {
    let state = applyPointer(emptySelection(), 7, {}, TOTAL, idAt);
    state = applyPointer(state, 4, { shift: true }, TOTAL, idAt);
    expect(ids(state)).toEqual(["id-4", "id-5", "id-6", "id-7"]);
  });

  it("Ctrl+Shift+クリックは既存選択に範囲を追加する", () => {
    let state = applyPointer(emptySelection(), 3, {}, TOTAL, idAt);
    state = applyPointer(state, 7, { ctrl: true }, TOTAL, idAt);
    state = applyPointer(state, 1, { ctrl: true, shift: true }, TOTAL, idAt);
    // id-7..id-1 の範囲がid-3, id-7に追加される
    expect(ids(state)).toEqual([
      "id-1",
      "id-2",
      "id-3",
      "id-4",
      "id-5",
      "id-6",
      "id-7",
    ]);
  });

  it("アンカーなしのShift+クリックは単一選択として扱う", () => {
    const state = applyPointer(emptySelection(), 5, { shift: true }, TOTAL, idAt);
    expect(ids(state)).toEqual(["id-5"]);
    expect(state.anchorIndex).toBe(5);
  });

  it("範囲外のクリックは無視する", () => {
    const before = applyPointer(emptySelection(), 3, {}, TOTAL, idAt);
    expect(applyPointer(before, -1, {}, TOTAL, idAt)).toBe(before);
    expect(applyPointer(before, TOTAL, {}, TOTAL, idAt)).toBe(before);
  });

  it("未ロード (id null) のクリックは選択を変えない", () => {
    const before = applyPointer(emptySelection(), 3, {}, TOTAL, idAt);
    const state = applyPointer(before, 9, {}, TOTAL, () => null);
    expect(ids(state)).toEqual(["id-3"]);
  });
});

describe("moveIndexFrom — キーボード移動のindex計算", () => {
  const opts = { total: TOTAL, columns: COLUMNS, pageRows: 3 };

  it("左右移動は行端で折返す", () => {
    // 7 = row1 col2。Right×3 → row1 col4=9 → wrap → row2 col0=10
    expect(moveIndexFrom("ArrowRight", 9, opts)).toBe(10);
    // 10 = row2 col0。Left → row1 col4 = 9
    expect(moveIndexFrom("ArrowLeft", 10, opts)).toBe(9);
  });

  it("先頭・末尾でクランプする", () => {
    expect(moveIndexFrom("ArrowLeft", 0, opts)).toBe(0);
    expect(moveIndexFrom("ArrowUp", 2, opts)).toBe(2);
    expect(moveIndexFrom("ArrowRight", TOTAL - 1, opts)).toBe(TOTAL - 1);
    expect(moveIndexFrom("ArrowDown", TOTAL - 1, opts)).toBe(TOTAL - 1);
  });

  it("上下は列数分移動する", () => {
    expect(moveIndexFrom("ArrowDown", 7, opts)).toBe(12);
    expect(moveIndexFrom("ArrowUp", 7, opts)).toBe(2);
    // 末尾行ではクランプ
    expect(moveIndexFrom("ArrowDown", 18, opts)).toBe(19);
  });

  it("Home/Endは先頭・末尾へ", () => {
    expect(moveIndexFrom("Home", 13, opts)).toBe(0);
    expect(moveIndexFrom("End", 3, opts)).toBe(TOTAL - 1);
  });

  it("PageUp/PageDownはpageRows×columns移動する", () => {
    expect(moveIndexFrom("PageDown", 0, opts)).toBe(15);
    expect(moveIndexFrom("PageUp", 19, opts)).toBe(4);
    // クランプ
    expect(moveIndexFrom("PageDown", 17, opts)).toBe(19);
    expect(moveIndexFrom("PageUp", 3, opts)).toBe(0);
  });

  it("total=0では移動しない", () => {
    expect(moveIndexFrom("ArrowRight", 0, { ...opts, total: 0 })).toBe(0);
  });
});

describe("applyKey — キーボード選択", () => {
  const opts = { total: TOTAL, columns: COLUMNS, pageRows: 3 };

  it("矢印キーはフォーカスを移動し単一選択する (アンカー更新)", () => {
    const state = applyKey(emptySelection(), "ArrowRight", { ...opts, shift: false }, idAt);
    expect(ids(state)).toEqual(["id-1"]);
    expect(state.anchorIndex).toBe(1);
    expect(state.focusIndex).toBe(1);
  });

  it("フォーカスのない状態からの矢印は先頭を選択する", () => {
    const state = applyKey(emptySelection(), "ArrowDown", { ...opts, shift: false }, idAt);
    expect(state.focusIndex).toBe(COLUMNS);
    expect(ids(state)).toEqual([`id-${COLUMNS}`]);
  });

  it("Shift+矢印はアンカーからの範囲を拡張する", () => {
    let state = applyKey(emptySelection(), "ArrowRight", { ...opts, shift: false }, idAt);
    state = applyKey(state, "ArrowRight", { ...opts, shift: true }, idAt);
    expect(ids(state)).toEqual(["id-1", "id-2"]);
    state = applyKey(state, "ArrowRight", { ...opts, shift: true }, idAt);
    expect(ids(state)).toEqual(["id-1", "id-2", "id-3"]);
    // アンカー不変
    expect(state.anchorIndex).toBe(1);
    expect(state.focusIndex).toBe(3);
  });

  it("Shift+上矢印で逆方向へ拡張する", () => {
    let state = applyPointer(emptySelection(), 7, {}, TOTAL, idAt);
    state = applyKey(state, "ArrowUp", { ...opts, shift: true }, idAt);
    expect(ids(state)).toEqual(["id-2", "id-3", "id-4", "id-5", "id-6", "id-7"]);
  });

  it("未ロード項目へ移動してもフォーカスは進む (選択集合は変わらない)", () => {
    let state = applyPointer(emptySelection(), 3, {}, TOTAL, idAt);
    state = applyKey(state, "ArrowRight", { ...opts, shift: false }, (i) =>
      i === 4 ? null : `id-${i}`,
    );
    expect(state.focusIndex).toBe(4);
    expect(state.anchorIndex).toBe(4);
    expect(ids(state)).toEqual(["id-3"]);
  });

  it("total=0では何もしない", () => {
    const state = applyKey(emptySelection(), "ArrowRight", { ...opts, total: 0, shift: false }, idAt);
    expect(state.focusIndex).toBeNull();
    expect(state.selected.size).toBe(0);
  });
});
