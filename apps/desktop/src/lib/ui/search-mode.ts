/**
 * UIUX-7: 意味検索の入口となる検索モード (3値) の純粋関数群。
 *
 * - 従来の連続スライダー (0..100%) を廃止し、モードは keyword / hybrid /
 *   semantic の3値のみ。実行時の意味重みは MODE_WEIGHT_PERCENT で導出する
 *   (backend契約のweight 0..1・search_run APIは不変)。
 * - 永続化はlocalStorage (KeyValueStorage注入 — LayoutPrefsStore慣行)。
 *   破損・未知値はkeyword (現行互換の既定) へフォールバックする。
 * - searchBadgeLabel は折りたたみ検索行のbadge表示用 (類似検索は「類似」)。
 */

import type { KeyValueStorage } from "./layout-prefs";

/** 検索モード (意味検索の効かせ方)。 */
export type SemanticMode = "keyword" | "hybrid" | "semantic";

/** モード → 実行時の意味重み (%)。キーワード=0 / ハイブリッド=50 / 意味=100。 */
export const MODE_WEIGHT_PERCENT: Record<SemanticMode, number> = {
  keyword: 0,
  hybrid: 50,
  semantic: 100,
};

/** localStorage永続化キー。 */
export const SEMANTIC_MODE_STORAGE_KEY = "gento.search.semanticMode";

/** 永続化値の検証 (破損・未知値はkeywordへ — 前方互換)。 */
export function coerceSemanticMode(value: unknown): SemanticMode {
  return value === "hybrid" || value === "semantic" ? value : "keyword";
}

/** 実行時の意味重み (%) → モード (0=keyword / 100=semantic / それ以外=hybrid)。 */
export function semanticModeOfWeightPercent(percent: number): SemanticMode {
  if (percent >= 100) {
    return "semantic";
  }
  if (percent > 0) {
    return "hybrid";
  }
  return "keyword";
}

/** モードのUIラベル (セグメントボタン・折りたたみbadge共通)。 */
export function semanticModeLabel(mode: SemanticMode): string {
  return mode === "semantic" ? "意味" : mode === "hybrid" ? "ハイブリッド" : "キーワード";
}

/** 折りたたみ検索行のmode badge (類似画像検索は重みによらず「類似」)。 */
export function searchBadgeLabel(
  kind: "query" | "similar",
  weightPercent: number,
): string {
  if (kind === "similar") {
    return "類似";
  }
  return semanticModeLabel(semanticModeOfWeightPercent(weightPercent));
}

/** 検索モードの保存・復元 (単一文字列を素のまま保存 — JSON不要)。 */
export class SemanticModeStore {
  constructor(private readonly storage: KeyValueStorage) {}

  load(): SemanticMode {
    return coerceSemanticMode(this.storage.getItem(SEMANTIC_MODE_STORAGE_KEY));
  }

  save(mode: SemanticMode): void {
    this.storage.setItem(SEMANTIC_MODE_STORAGE_KEY, mode);
  }
}
