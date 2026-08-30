/**
 * 比較スロットの充填・進行の純粋ロジック (WP-6 / フェーズ7、spec §9.4)。
 *
 * - 充填: グリッド選択の先頭N件。不足分は現在位置 (focusIndex) から連続補充
 * - 次グループ: 基準 (base) スロットは画像を保持し、他スロットだけ進む
 * - nextIndex: 次グループが読み込みを始めるカタログindex
 */

export type SlotItem = {
  assetId: string;
  displayName: string;
  width: number;
  height: number;
  /** 内容感応URLバージョン (ページDTO由来。FG-1)。 */
  version: number | null;
};

export type SlotSourceItem = {
  id: string;
  displayName: string;
  width: number | null;
  height: number | null;
  state: string;
  /** 内容感応URLバージョン (ページDTO由来。FG-1)。 */
  version: number | null;
};

export type SlotGet = (index: number) => SlotSourceItem | null;

function toSlotItem(item: SlotSourceItem): SlotItem {
  return {
    assetId: item.id,
    displayName: item.displayName,
    width: item.width ?? 0,
    height: item.height ?? 0,
    version: item.version,
  };
}

export type FillSlotsInput = {
  /** 選択順のID列 (Setの挿入順を想定)。 */
  selectedIds: readonly string[];
  count: number;
  /** 現在位置 (グリッドのフォーカスindex)。 */
  focusIndex: number | null;
  total: number;
  get: SlotGet;
  /** キャッシュ済みページからid→indexを解決する (見つからないnull)。 */
  findIndexById(id: string): number | null;
};

export type FillResult = {
  slots: Array<SlotItem | null>;
  /** 充填に使用したindexのうち最大のもの + 1 (次グループの開始位置)。 */
  nextIndex: number;
};

/** 選択の先頭N件 + 不足分を現在位置から連続補充してスロットを埋める。 */
export function fillSlots(input: FillSlotsInput): FillResult {
  const { selectedIds, count, focusIndex, total, get, findIndexById } = input;
  const slots: Array<SlotItem | null> = new Array(count).fill(null);
  const used = new Set<string>();
  let maxIndex = -1;

  const tryPush = (index: number): boolean => {
    const item = get(index);
    if (item === null || item.state !== "ready" || item.width === null || item.height === null) {
      return false;
    }
    if (used.has(item.id)) {
      return false;
    }
    const free = slots.findIndex((slot) => slot === null);
    if (free < 0) {
      return false;
    }
    slots[free] = toSlotItem(item);
    used.add(item.id);
    maxIndex = Math.max(maxIndex, index);
    return true;
  };

  for (const id of selectedIds) {
    if (!slots.some((slot) => slot === null)) {
      break;
    }
    const index = findIndexById(id);
    if (index !== null && index >= 0 && index < total) {
      tryPush(index);
    }
  }

  if (slots.some((slot) => slot === null)) {
    const start = total > 0 && focusIndex !== null ? Math.min(Math.max(focusIndex, 0), total - 1) : 0;
    for (let index = start; index < total && slots.some((slot) => slot === null); index++) {
      tryPush(index);
    }
  }

  return { slots, nextIndex: Math.max(maxIndex + 1, 0) };
}

/**
 * 次グループへ進める。基準スロット (base) は画像を保持し、
 * 他スロットに nextIndex から連続するready項目を割り当てる。
 * 基準と同じassetIdは他スロットへ割り当てない。終端では既存を維持。
 */
export function nextGroup(
  slots: Array<SlotItem | null>,
  nextIndex: number,
  base: number | null,
  total: number,
  get: SlotGet,
): FillResult {
  const result = [...slots];
  const baseId = base !== null ? (result[base]?.assetId ?? null) : null;
  let cursor = nextIndex;
  let lastAssigned = nextIndex - 1;

  for (let i = 0; i < result.length; i++) {
    if (base !== null && i === base) {
      continue;
    }
    while (cursor < total) {
      const index = cursor++;
      const item = get(index);
      if (item === null || item.state !== "ready" || item.width === null || item.height === null) {
        continue;
      }
      if (baseId !== null && item.id === baseId) {
        continue;
      }
      result[i] = toSlotItem(item);
      lastAssigned = index;
      break;
    }
  }

  return { slots: result, nextIndex: Math.max(cursor, lastAssigned + 1) };
}

/** 2枚比較の左右入れ替え。 */
export function swapSlots<T>(slots: T[]): T[] {
  return [...slots].reverse();
}
