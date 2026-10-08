/**
 * UIUX-7: 意味検索サジェストチップ「🧠 意味検索を試す」の表示判定 (純粋関数)。
 *
 * 検索**実行後**の確定済みクエリ (lastSearchMeta) に対してのみ判定する
 * (入力中の逐次表示はしない — タイピングノイズ防止)。すべて満たすときtrue:
 * ①semanticAvailable ②weightPercent===0 (キーワードモードで実行)
 * ③query.trim()非empty ④構文トークン ([A-Za-z_]+:) を含まない —
 * tag:/rating:/type:等のフィールド構文入力はキーワード検索の意図のため
 * 意味検索のサジェストを出さない。⑤queryが却下済みクエリと一致しない
 * (チップclick後の再実行が失敗/遅延しても旧メタで出し続けない — WP3)。
 * ⑥検索実行中でない (再実行中の点滅防止 — WP3)。新しいクエリの検索では
 * ⑤の一致が外れるため自然に再表示される。⑦検索結果表示中である
 * (WP4 — exitSearchはlastSearchMetaを残すため、searchActive=falseで
 * 検索終了後の表示を断つ)。
 */

/** フィールド構文トークン (tag:・rating:・added:・model: 等)。 */
const FIELD_TOKEN_PATTERN = /[A-Za-z_]+:/;

/** 追加表示条件 (WP3/WP4 — すべて省略時は従来の4条件のみ)。 */
export type SemanticSuggestContext = {
  /** チップclickで却下されたクエリ (同一クエリのサジェストは出さない)。 */
  dismissedQuery?: string | null;
  /** 検索実行中なら出さない (再実行完了後に確定値で再判定される)。 */
  running?: boolean;
  /** 検索結果表示中のみ出す (exitSearchはlastSearchMetaを残すため終了後は非表示)。 */
  searchActive?: boolean;
};

export function suggestsSemantic(
  query: string,
  weightPercent: number,
  semanticAvailable: boolean,
  context: SemanticSuggestContext = {},
): boolean {
  return (
    !context.running &&
    context.searchActive !== false &&
    semanticAvailable &&
    weightPercent === 0 &&
    query.trim() !== "" &&
    !FIELD_TOKEN_PATTERN.test(query) &&
    query !== context.dismissedQuery
  );
}
