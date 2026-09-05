/**
 * UIUX-3 V-3: 検索行折りたたみ時の1行要約 (純粋関数)。
 *
 * - 要約は**確定済み検索状態** (最終実行のquery/kind/scope) から構築する。
 *   `searchInput` (入力途中ドラフト) は使わない — 折りたたみ中に見えるのは
 *   「今グリッドに表示されている検索」の条件でなければ困惑するため。
 * - 類似画像検索は検索語の代わりに固定表示「類似画像」。
 * - scopeは実態名 (「登録ルート: ‹path›」「コレクション: ‹名前›」
 *   「ライブラリ全体」) をそのまま使う — 「現在のフォルダ」は実際の検索範囲
 *   (登録ルート全体) と不一致のため使わない (UIUX-3計画書の裁定)。
 * - 検索実行中・検索結果表示中はクリア (解除) 操作を折りたたみでも見せる。
 */

export type CommittedSearchState = {
  /** 最終実行の検索種別 (未実行=none)。 */
  kind: "none" | "query" | "similar";
  /** 最終実行のクエリ文字列 (kind=queryのとき表示)。 */
  query: string;
  /** 最終実行のscope実態名ラベル (未実行=nullで非表示)。 */
  scopeLabel: string | null;
  /** 最終実行時の意味重み (0..100)。 */
  semanticWeightPercent: number;
  /** 最終実行時にGUIフィルターが有効だったか。 */
  filtersActive: boolean;
  /** 検索が実行中か。 */
  running: boolean;
  /** 検索結果表示中か (カタログがsearchソース)。 */
  active: boolean;
};

export type CollapsedSearchSummary = {
  /** 検索語表示 (類似画像は「類似画像」。未実行="")。 */
  queryText: string;
  /** scope badge文言 (未実行=nullで非表示)。 */
  scopeLabel: string | null;
  /** 追加badge (フィルター・意味重み)。 */
  badges: string[];
  /** 折りたたみ行にクリア (解除) 操作を出すか。 */
  showClear: boolean;
};

export function collapsedSearchSummary(state: CommittedSearchState): CollapsedSearchSummary {
  const badges: string[] = [];
  if (state.filtersActive) {
    badges.push("フィルター");
  }
  if (state.semanticWeightPercent > 0) {
    badges.push(`意味 ${state.semanticWeightPercent}%`);
  }
  return {
    queryText: state.kind === "similar" ? "類似画像" : state.kind === "query" ? state.query : "",
    scopeLabel: state.kind === "none" ? null : state.scopeLabel,
    badges,
    showClear: state.running || state.active,
  };
}
