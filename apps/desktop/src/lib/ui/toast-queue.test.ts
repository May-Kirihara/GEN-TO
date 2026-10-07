import { describe, expect, it } from "vitest";
import { EMPTY_TOAST_QUEUE, enqueueToast, withoutToast, type ToastQueue } from "./toast-queue";

describe("enqueueToast (UIUX-5/WP9: トーストキュー純粋遷移)", () => {
  it("別textのpushは時系列順に2エントリ", () => {
    const queue = enqueueToast(enqueueToast(EMPTY_TOAST_QUEUE, "meta", "評価を適用しました"), "os", "パスをコピーしました");
    expect(queue.toasts).toEqual([
      { id: 1, kind: "meta", text: "評価を適用しました" },
      { id: 2, kind: "os", text: "パスをコピーしました" },
    ]);
  });

  it("同一textの連続pushも別idで2エントリが生存 (同一文言再通知でもトーストは再表示)", () => {
    const queue = enqueueToast(enqueueToast(EMPTY_TOAST_QUEUE, "os", "パスをコピーしました (1件)"), "os", "パスをコピーしました (1件)");
    expect(queue.toasts.length).toBe(2);
    expect(queue.toasts[0]?.id).not.toBe(queue.toasts[1]?.id);
    expect(queue.toasts.map((toast) => toast.text)).toEqual([
      "パスをコピーしました (1件)",
      "パスをコピーしました (1件)",
    ]);
  });

  it("idはpushごとに一意 (同一text混在の連続pushで重複なし)", () => {
    let queue: ToastQueue = EMPTY_TOAST_QUEUE;
    for (let i = 0; i < 5; i += 1) {
      queue = enqueueToast(queue, "meta", "対象はすべて処理済みです");
    }
    const ids = queue.toasts.map((toast) => toast.id);
    expect(new Set(ids).size).toBe(ids.length);
    expect(queue.seq).toBe(5);
  });

  it("dismiss後のpushは消失idを再利用しない (一意性はキュー全体生涯で保証)", () => {
    let queue = enqueueToast(EMPTY_TOAST_QUEUE, "os", "1回目");
    const firstId = queue.seq;
    queue = withoutToast(queue, firstId);
    queue = enqueueToast(queue, "os", "1回目");
    expect(queue.toasts).toEqual([{ id: 2, kind: "os", text: "1回目" }]);
    expect(queue.seq).toBe(2);
  });
});

describe("withoutToast (UIUX-5/WP9: トースト除去)", () => {
  it("該当idのみ除去し他は保持", () => {
    let queue = enqueueToast(EMPTY_TOAST_QUEUE, "meta", "1");
    queue = enqueueToast(queue, "os", "2");
    queue = enqueueToast(queue, "meta", "3");
    const removed = withoutToast(queue, queue.toasts[1]!.id);
    expect(removed.toasts.map((toast) => toast.text)).toEqual(["1", "3"]);
    // seq (次id) はdismissで巻き戻さない
    expect(removed.seq).toBe(queue.seq);
  });

  it("存在しないidでは等価なキューを返す", () => {
    const queue = enqueueToast(EMPTY_TOAST_QUEUE, "meta", "1");
    expect(withoutToast(queue, 999).toasts).toEqual(queue.toasts);
  });
});
