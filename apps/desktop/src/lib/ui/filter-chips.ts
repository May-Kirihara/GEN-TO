/**
 * UIUX-5/WP2: 検索フィルタのチップ表示 (純粋関数)。
 *
 * - チップは**ドラフト値** (`searchFilters` 現値) から構築する — 編集UIの一部で
 *   あり、折りたたみ行のsummary (確定値 = lastSearchMeta) とは別物。
 * - 追加日範囲 (addedFrom/addedTo) は1チップ (key="addedRange") にまとめる。
 *   ×解除で両端を一緒に既定値へ戻す。
 * - `filtersActive` とチップありは一致する (favorite=false も有効な絞り込み
 *   ため「♥ お気に入り以外」としてチップ化する)。
 */
import type { GuiFilters } from "../search";

/** チップの識別子 (GuiFiltersのフィールドに相当。追加日のみ範囲単位)。 */
export type FilterChipKey = "ratingMin" | "cull" | "favorite" | "type" | "tag" | "addedRange";

export type FilterChip = {
  key: FilterChipKey;
  label: string;
};

const CULL_LABELS: Record<NonNullable<GuiFilters["cull"]>, string> = {
  picked: "採用",
  rejected: "除外",
  unselected: "未選別",
};

/** 設定済みフィルタをチップ列挙 (順序固定: 評価→選別→♥→形式→タグ→追加日)。 */
export function filterChips(filters: GuiFilters): FilterChip[] {
  const chips: FilterChip[] = [];
  if (filters.ratingMin !== null) {
    chips.push({ key: "ratingMin", label: `★${filters.ratingMin}以上` });
  }
  if (filters.cull !== null) {
    chips.push({ key: "cull", label: CULL_LABELS[filters.cull] });
  }
  if (filters.favorite !== null) {
    chips.push({
      key: "favorite",
      label: filters.favorite ? "♥ お気に入り" : "♥ お気に入り以外",
    });
  }
  if (filters.type !== "") {
    chips.push({ key: "type", label: filters.type.toUpperCase() });
  }
  if (filters.tag !== "") {
    chips.push({ key: "tag", label: `タグ:${filters.tag.trim()}` });
  }
  if (filters.addedFrom !== "" || filters.addedTo !== "") {
    chips.push({ key: "addedRange", label: `追加日 ${filters.addedFrom}〜${filters.addedTo}` });
  }
  return chips;
}

/** チップの×操作: 当該フィルタのみ既定値 (EMPTY_FILTERS値) へ戻したコピー。 */
export function withoutFilter(filters: GuiFilters, key: FilterChipKey): GuiFilters {
  switch (key) {
    case "ratingMin":
      return { ...filters, ratingMin: null };
    case "cull":
      return { ...filters, cull: null };
    case "favorite":
      return { ...filters, favorite: null };
    case "type":
      return { ...filters, type: "" };
    case "tag":
      return { ...filters, tag: "" };
    case "addedRange":
      return { ...filters, addedFrom: "", addedTo: "" };
  }
}
