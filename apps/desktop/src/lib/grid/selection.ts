/**
 * 選択状態の純粋ロジック (WP-5 / 本体計画 §10、spec §18.2)。
 *
 * - 状態: 選択集合 (IDのSet) + アンカーindex + フォーカスindex
 * - 単一/Ctrl/Shiftクリック、矢印・Home/End/PageUp/PageDown、Shift+矢印
 * - ID解決は idAt 注入 (ページ未取得のindexはnull。フォーカスは進み選択集合は不変)
 * - Svelteコンポーネントには結果のスナップショットを再代入して使う
 */

export type SelectionState = {
  /** 選択済み項目ID。 */
  selected: ReadonlySet<string>;
  /** 範囲選択の起点index。 */
  anchorIndex: number | null;
  /** キーボードフォーカスindex。 */
  focusIndex: number | null;
};

export type PointerModifiers = { ctrl?: boolean; shift?: boolean };

export type MoveAction =
  | "ArrowLeft"
  | "ArrowRight"
  | "ArrowUp"
  | "ArrowDown"
  | "Home"
  | "End"
  | "PageUp"
  | "PageDown";

export function emptySelection(): SelectionState {
  return { selected: new Set(), anchorIndex: null, focusIndex: null };
}

function rangeIds(from: number, to: number, idAt: (index: number) => string | null): string[] {
  const start = Math.min(from, to);
  const end = Math.max(from, to);
  const ids: string[] = [];
  for (let index = start; index <= end; index++) {
    const id = idAt(index);
    if (id !== null) {
      ids.push(id);
    }
  }
  return ids;
}

/** ポインタ選択。範囲外のindexは現在状態を返す (不変同一参照)。 */
export function applyPointer(
  state: SelectionState,
  index: number,
  modifiers: PointerModifiers,
  total: number,
  idAt: (index: number) => string | null,
): SelectionState {
  if (index < 0 || index >= total) {
    return state;
  }
  const ctrl = modifiers.ctrl ?? false;
  const shift = modifiers.shift ?? false;
  const id = idAt(index);

  if (shift && state.anchorIndex !== null) {
    const anchor = state.anchorIndex;
    const range = rangeIds(anchor, index, idAt);
    let next: Set<string>;
    if (ctrl) {
      // Ctrl+Shift: 既存選択に範囲を追加
      next = new Set(state.selected);
      for (const rangeId of range) {
        next.add(rangeId);
      }
    } else {
      next = new Set(range);
    }
    return { selected: next, anchorIndex: anchor, focusIndex: index };
  }

  if (ctrl && id !== null) {
    const next = new Set(state.selected);
    if (next.has(id)) {
      next.delete(id);
    } else {
      next.add(id);
    }
    return { selected: next, anchorIndex: index, focusIndex: index };
  }

  // 単一選択。id未解決 (ページ未取得) のときはフォーカスだけ進め選択集合を保持する
  if (id === null) {
    return { selected: state.selected, anchorIndex: index, focusIndex: index };
  }
  return {
    selected: new Set([id]),
    anchorIndex: index,
    focusIndex: index,
  };
}

/** キー操作後の移動先index (折返し・クランプ込み)。 */
export function moveIndexFrom(
  action: MoveAction,
  index: number,
  opts: { total: number; columns: number; pageRows: number },
): number {
  const { total, columns, pageRows } = opts;
  if (total <= 0 || columns <= 0) {
    return index;
  }
  const last = total - 1;
  switch (action) {
    case "ArrowLeft":
      return Math.max(0, index - 1);
    case "ArrowRight":
      return Math.min(last, index + 1);
    case "ArrowUp":
      // 先頭行では列を保存したまま移動しない (index 0へ寄せない)
      return index < columns ? index : index - columns;
    case "ArrowDown":
      return Math.min(last, index + columns);
    case "Home":
      return 0;
    case "End":
      return last;
    case "PageUp":
      return Math.max(0, index - pageRows * columns);
    case "PageDown":
      return Math.min(last, index + pageRows * columns);
  }
}

/** キーボード選択。total=0では現在状態を返す。 */
export function applyKey(
  state: SelectionState,
  action: MoveAction,
  opts: { shift: boolean; total: number; columns: number; pageRows: number },
  idAt: (index: number) => string | null,
): SelectionState {
  if (opts.total <= 0) {
    return state;
  }
  const from = state.focusIndex ?? 0;
  const to = moveIndexFrom(action, from, opts);

  if (opts.shift && state.anchorIndex !== null) {
    // Shift+移動: アンカーからの範囲を選択へ追加 (拡張のみ)
    const next = new Set(state.selected);
    for (const id of rangeIds(state.anchorIndex, to, idAt)) {
      next.add(id);
    }
    return { selected: next, anchorIndex: state.anchorIndex, focusIndex: to };
  }

  const id = idAt(to);
  // id未解決 (ページ未取得) のときはフォーカスだけ進め選択集合を保持する
  if (id === null) {
    return { selected: state.selected, anchorIndex: to, focusIndex: to };
  }
  return {
    selected: new Set([id]),
    anchorIndex: to,
    focusIndex: to,
  };
}
