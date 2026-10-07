/**
 * UIUX-5/WP9: トーストキューの状態遷移 (純粋関数)。
 *
 * - pushは毎回新しいidを割り当てる — 同一textの連続通知も別エントリとして
 *   生存させる。$effectミラー (空→非空遷移で発火) では同一文言の再設定を
 *   検出できず、effect内からのpushはtoastQueueをread+writeして更新深度
 *   超過まで無限pushする潜在バグになるため、WP10で全kindのミラーを廃止し
 *   設定箇所からの明示pushに統一した (Codex review 2026-10-07 2・3回目
 *   blocker対策)。
 * - 寿命管理 (4秒自動消滅のsetTimeout・タイマー解除) はAppが正本 — この
 *   moduleはキューの状態遷移のみを担う (純粋性維持)。
 * - `seq` は「最後に割り当てたid」= 次回pushの基点。dismissでは巻き戻さ
 *   ない (id一意性はキュー全体生涯で保証)。
 */

/** トーストの発生源。全kindとも設定箇所 (AppのshowXxxNoticeヘルパー) が明示pushする。 */
export type ToastKind = "viewer" | "fileops" | "meta" | "os";

export type ToastEntry = { id: number; kind: ToastKind; text: string };

export type ToastQueue = { seq: number; toasts: ToastEntry[] };

export const EMPTY_TOAST_QUEUE: ToastQueue = { seq: 0, toasts: [] };

/** トーストを末尾へ追加 (同一textでも毎回別idを割り当てる)。 */
export function enqueueToast(queue: ToastQueue, kind: ToastKind, text: string): ToastQueue {
  const id = queue.seq + 1;
  return { seq: id, toasts: [...queue.toasts, { id, kind, text }] };
}

/** 該当idのみ除去する (存在しないidなら等価なキューを返す)。 */
export function withoutToast(queue: ToastQueue, id: number): ToastQueue {
  return { seq: queue.seq, toasts: queue.toasts.filter((toast) => toast.id !== id) };
}
