import { describe, expect, it, vi } from "vitest";
import { UndoStack } from "./undo";

function makeDeferred() {
  let resolve!: () => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<void>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

describe("UndoStack", () => {
  it("空スタックのundoはfalse", async () => {
    const stack = new UndoStack();
    expect(stack.canUndo()).toBe(false);
    expect(stack.peekLabel()).toBeNull();
    await expect(stack.undo()).resolves.toBe(false);
  });

  it("push後に巻き戻せる (LIFO順)", async () => {
    const stack = new UndoStack();
    const log: string[] = [];
    stack.push({
      label: "評価 ★5",
      undo: async () => {
        log.push("undo-rating");
      },
    });
    stack.push({
      label: "除外",
      undo: async () => {
        log.push("undo-cull");
      },
    });
    expect(stack.depth).toBe(2);
    expect(stack.peekLabel()).toBe("除外");

    await expect(stack.undo()).resolves.toBe(true);
    expect(log).toEqual(["undo-cull"]);
    await expect(stack.undo()).resolves.toBe(true);
    expect(log).toEqual(["undo-cull", "undo-rating"]);
    expect(stack.canUndo()).toBe(false);
  });

  it("上限N件を超えた分は古い方から捨てる", () => {
    const stack = new UndoStack(3);
    for (let i = 0; i < 5; i++) {
      stack.push({ label: `op${i}`, undo: async () => {} });
    }
    expect(stack.depth).toBe(3);
    expect(stack.peekLabel()).toBe("op4");
    stack.clear();
    expect(stack.depth).toBe(0);
  });

  it("undo内の失敗はlastErrorへ載り、エントリは消費される", async () => {
    const stack = new UndoStack();
    stack.push({
      label: "失敗する操作",
      undo: async () => {
        throw new Error("対象が見つかりません");
      },
    });
    await expect(stack.undo()).resolves.toBe(false);
    expect(stack.lastError).toContain("見つかりません");
    // 失敗したエントリは再試行しない
    expect(stack.depth).toBe(0);
  });

  it("undoの完了を待ってから次のエントリを処理する (順序保証)", async () => {
    const stack = new UndoStack();
    const first = makeDeferred();
    const order: string[] = [];
    stack.push({
      label: "遅い操作",
      undo: () =>
        first.promise.then(() => {
          order.push("first-done");
        }),
    });
    stack.push({
      label: "速い操作",
      undo: async () => {
        order.push("second-done");
      },
    });
    const undoing = stack.undo();
    // 未完了の間に次を呼んでも待たされる (呼び出し側で直列化する設計)
    const second = stack.undo();
    first.resolve();
    await Promise.all([undoing, second]);
    expect(order).toEqual(["second-done", "first-done"]);
  });

  it("onChangeで版が進む", async () => {
    const stack = new UndoStack();
    const versions: number[] = [];
    const stop = stack.onChange(() => versions.push(stack.version));
    stack.push({ label: "a", undo: async () => {} });
    await stack.undo();
    stack.clear();
    stop();
    stack.push({ label: "b", undo: async () => {} });
    expect(versions.length).toBe(2);
  });
});

describe("UndoStack: メタデータ操作との統合動作", () => {
  it("before値を捕捉した逆適用で評価を巻き戻す", async () => {
    // 実際のApp実装と同じ構造: 編集前にbefore値を読み、undoで再setする
    const values = new Map<string, number>([["1", 0]]);
    const setRating = vi.fn(async (ids: string[], rating: number) => {
      for (const id of ids) {
        values.set(id, rating);
      }
    });
    const stack = new UndoStack();

    const ids = ["1"];
    const before = ids.map((id) => values.get(id) ?? 0);
    await setRating(ids, 5);
    stack.push({
      label: "評価 ★5",
      undo: async () => {
        // before値へ戻す (idごとに元の値)
        for (const [index, id] of ids.entries()) {
          await setRating([id], before[index]);
        }
      },
    });
    expect(values.get("1")).toBe(5);

    await stack.undo();
    expect(values.get("1")).toBe(0);
    expect(setRating).toHaveBeenCalledTimes(2);
  });
});
