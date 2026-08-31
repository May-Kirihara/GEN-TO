/**
 * 高速選別モードのキー対応 (MVP WP-5 / 仕様§9.2)。
 *
 * ビューア内で:
 * - 数字 1〜5 = 星評価
 * - 0 = 評価解除 (0星へ)
 * - X = 除外 (rejected)
 * - P または A = 採用 (picked)
 * 入力後、自動的に次の画像へ送る (自動次送り)。トグルでON/OFFでき、
 * OFF時は既存のズームキー (0=fit / 1=actual) が優先される。
 *
 * 純粋関数のみ — UI側 (ImageViewer) がキーイベントをここへ通し、
 * 返った操作をMetadataStoreへ適用する。
 */

export type TriageKeyAction =
  | { kind: "rating"; value: number }
  | { kind: "cull"; value: "picked" | "rejected" };

/**
 * 高速選別モードのキーを操作へ変換する。該当なしはnull。
 * 修飾キー付き (Ctrl/Meta/Alt) はOS・ブラウザ操作と衝突するため対象外。
 */
export function parseTriageKey(
  key: string,
  opts?: { ctrlKey?: boolean; metaKey?: boolean; altKey?: boolean },
): TriageKeyAction | null {
  if (opts?.ctrlKey || opts?.metaKey || opts?.altKey) {
    return null;
  }
  switch (key) {
    case "1":
    case "2":
    case "3":
    case "4":
    case "5":
      return { kind: "rating", value: Number(key) };
    case "0":
      return { kind: "rating", value: 0 };
    case "x":
    case "X":
      return { kind: "cull", value: "rejected" };
    case "p":
    case "P":
    case "a":
    case "A":
      return { kind: "cull", value: "picked" };
    default:
      return null;
  }
}

/** Undo通知等に使う操作の短いラベル。 */
export function triageActionLabel(action: TriageKeyAction): string {
  if (action.kind === "rating") {
    return action.value === 0 ? "評価解除" : `評価 ★${action.value}`;
  }
  return action.value === "picked" ? "採用" : "除外";
}
