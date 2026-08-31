/**
 * lib/fileops.ts の純粋関数の検証 (WP-7)。
 *
 * 衝突セッションの全解決組み合わせ (上書き/スキップ/両方保持 ×
 * 単発/以降全部) と、部分成功の集計・再試行引数を検証する。
 */
import { describe, expect, it } from "vitest";
import {
  applyConflictChoice,
  conflictSessionComplete,
  createConflictSession,
  describeKind,
  diskWarningOf,
  executeFileOp,
  outcomeNotice,
  planFileOp,
  resolutionsOf,
  retryArgsOf,
  summarizeOutcome,
  type FileOpOutcomeDto,
  type PlanStepDto,
} from "./fileops";

function step(fileId: string, name: string, conflict: boolean): PlanStepDto {
  return {
    file_id: fileId,
    src: `/r/${name}`,
    dst: `/r/dest/${name}`,
    display_name: name,
    conflict: conflict
      ? {
          dst: `/r/dest/${name}`,
          dst_byte_size: 10,
          dst_mtime_ms: 1000,
          hash_matches: true,
          keep_both_name: `(${fileId}) alt ${name}`,
        }
      : null,
    plan_error: null,
  };
}

describe("createConflictSession / applyConflictChoice", () => {
  it("衝突なし計画はcurrent=nullで即完了", () => {
    const session = createConflictSession([step("1", "a.png", false)]);
    expect(session.current).toBeNull();
    expect(session.pending).toHaveLength(0);
    expect(conflictSessionComplete(session)).toBe(true);
  });

  it("衝突3件: 先頭が表示対象・残り2", () => {
    const session = createConflictSession([
      step("1", "a.png", true),
      step("2", "b.png", true),
      step("3", "c.png", true),
    ]);
    expect(session.current?.file_id).toBe("1");
    expect(session.remaining).toBe(2);
    expect(conflictSessionComplete(session)).toBe(false);
  });

  it("単発選択で1件ずつ解決 (全解決組み合わせ)", () => {
    const choices = ["overwrite", "skip", "keep_both"] as const;
    for (const first of choices) {
      for (const second of choices) {
        for (const third of choices) {
          let session = createConflictSession([
            step("1", "a.png", true),
            step("2", "b.png", true),
            step("3", "c.png", true),
          ]);
          session = applyConflictChoice(session, first, false);
          expect(session.current?.file_id).toBe("2");
          session = applyConflictChoice(session, second, false);
          expect(session.current?.file_id).toBe("3");
          session = applyConflictChoice(session, third, false);
          expect(session.current).toBeNull();
          expect(conflictSessionComplete(session)).toBe(true);
          expect(resolutionsOf(session)).toEqual({
            "1": first,
            "2": second,
            "3": third,
          });
        }
      }
    }
  });

  it("applyAll: 以降すべてへ適用で残り全件が同一方針", () => {
    let session = createConflictSession([
      step("1", "a.png", true),
      step("2", "b.png", true),
      step("3", "c.png", true),
    ]);
    session = applyConflictChoice(session, "keep_both", true);
    expect(session.current).toBeNull();
    expect(conflictSessionComplete(session)).toBe(true);
    expect(resolutionsOf(session)).toEqual({
      "1": "keep_both",
      "2": "keep_both",
      "3": "keep_both",
    });
  });

  it("applyAllは未解決の後続にも適用 (解決済み前者は保持)", () => {
    let session = createConflictSession([
      step("1", "a.png", true),
      step("2", "b.png", true),
      step("3", "c.png", true),
    ]);
    session = applyConflictChoice(session, "skip", false);
    session = applyConflictChoice(session, "overwrite", true);
    expect(resolutionsOf(session)).toEqual({
      "1": "skip",
      "2": "overwrite",
      "3": "overwrite",
    });
  });
});

describe("summarizeOutcome / outcomeNotice", () => {
  const outcome: FileOpOutcomeDto = {
    op_id: "7",
    kind: "copy",
    status: "partial",
    undoable: true,
    steps: [
      {
        file_id: "1",
        from: "/r/a.png",
        to: "/r/d/a.png",
        status: "done",
        resolution: null,
        error: null,
        warning: null,
      },
      {
        file_id: "2",
        from: "/r/b.png",
        to: null,
        status: "skipped",
        resolution: "skip",
        error: null,
        warning: null,
      },
      {
        file_id: "3",
        from: "/r/c.png",
        to: null,
        status: "failed",
        resolution: null,
        error: "permission",
        warning: null,
      },
      {
        file_id: "4",
        from: "/r/d.png",
        to: null,
        status: "cancelled",
        resolution: null,
        error: "cancel",
        warning: null,
      },
    ],
  };

  it("成否を集計する", () => {
    expect(summarizeOutcome(outcome)).toEqual({
      done: 1,
      skipped: 1,
      failed: 1,
      cancelled: 1,
    });
  });

  it("トースト文に全状態とUndo可能性を含む", () => {
    const text = outcomeNotice(outcome);
    expect(text).toContain("コピー");
    expect(text).toContain("1件成功");
    expect(text).toContain("1件スキップ");
    expect(text).toContain("1件失敗");
    expect(text).toContain("1件中止");
    expect(text).toContain("Ctrl+Z");
  });

  it("trashはundoableでも完全削除はUndo案内しない", () => {
    const forever: FileOpOutcomeDto = {
      ...outcome,
      kind: "delete_forever",
      undoable: false,
    };
    expect(outcomeNotice(forever)).not.toContain("Ctrl+Z");
  });
});

describe("retryArgsOf", () => {
  it("失敗ステップのfromだけを再試行引数に組む", () => {
    const outcome: FileOpOutcomeDto = {
      op_id: "1",
      kind: "copy",
      status: "partial",
      undoable: true,
      steps: [
        {
          file_id: "1",
          from: "/r/a.png",
          to: "/r/d/a.png",
          status: "done",
          resolution: null,
          error: null,
          warning: null,
        },
        {
          file_id: "2",
          from: "/r/b.png",
          to: null,
          status: "failed",
          resolution: null,
          error: "x",
          warning: null,
        },
      ],
    };
    const retry = retryArgsOf(outcome, {
      kind: "copy",
      paths: ["/r/a.png", "/r/b.png"],
      dest_dir: "/r/d",
    });
    expect(retry).not.toBeNull();
    expect(retry?.paths).toEqual(["/r/b.png"]);
    expect(retry?.dest_dir).toBe("/r/d");
    expect(retry?.kind).toBe("copy");
  });

  it("失敗が無い場合はnull", () => {
    const outcome: FileOpOutcomeDto = {
      op_id: "1",
      kind: "trash",
      status: "done",
      undoable: true,
      steps: [],
    };
    expect(retryArgsOf(outcome, { kind: "trash", paths: ["/r/a.png"] })).toBeNull();
  });
});

describe("misc", () => {
  it("describeKindの全種別", () => {
    expect(describeKind("copy")).toBe("コピー");
    expect(describeKind("move")).toBe("移動");
    expect(describeKind("rename")).toBe("リネーム");
    expect(describeKind("mkdir")).toBe("新規フォルダ");
    expect(describeKind("trash")).toBe("ゴミ箱への移動");
    expect(describeKind("delete_forever")).toBe("完全削除");
  });

  it("容量不足警告の書式 (MiB / バイト)", () => {
    expect(diskWarningOf({ kind: "copy", steps: [], plan_errors: [], dest_root_id: null, disk_shortfall_bytes: null, confirm_token: null })).toBeNull();
    expect(
      diskWarningOf({ kind: "copy", steps: [], plan_errors: [], dest_root_id: null, disk_shortfall_bytes: 2 * 1024 * 1024, confirm_token: null }),
    ).toContain("2.0 MiB");
    expect(
      diskWarningOf({ kind: "copy", steps: [], plan_errors: [], dest_root_id: null, disk_shortfall_bytes: 100, confirm_token: null }),
    ).toContain("100 バイト");
  });
});

// ===== FG-WP7: 確認トークンの受け渡し・警告表示 =====

describe("確認トークン (FG-WP7)", () => {
  it("planFileOpは引数をsnake_caseへ変換して渡す (destDir/newName)", async () => {
    const calls: Array<[string, Record<string, unknown> | undefined]> = [];
    const ipc = {
      invoke: async <T>(command: string, args?: Record<string, unknown>): Promise<T> => {
        calls.push([command, args]);
        return { kind: "trash", steps: [], plan_errors: [], dest_root_id: null, disk_shortfall_bytes: null, confirm_token: null } as never;
      },
    };
    await planFileOp(ipc, { kind: "copy", paths: ["/r/a.png"], dest_dir: "/r/d", new_name: "b.png" });
    expect(calls[0]?.[0]).toBe("file_ops_plan");
    expect(calls[0]?.[1]).toEqual({
      kind: "copy",
      paths: ["/r/a.png"],
      destDir: "/r/d",
      newName: "b.png",
    });
  });

  it("executeFileOpはconfirm_tokenをargsへ含める", async () => {
    const calls: Array<[string, Record<string, unknown> | undefined]> = [];
    const ipc = {
      invoke: async <T>(command: string, args?: Record<string, unknown>): Promise<T> => {
        calls.push([command, args]);
        return {
          op_id: "1",
          kind: "delete_forever",
          status: "done",
          undoable: false,
          steps: [],
        } as never;
      },
    };
    await executeFileOp(ipc, {
      kind: "delete_forever",
      paths: ["/r/a.png"],
      confirm_token: "tok-1",
    });
    expect(calls[0]?.[0]).toBe("file_ops_execute");
    expect(calls[0]?.[1]).toEqual({
      args: { kind: "delete_forever", paths: ["/r/a.png"], confirm_token: "tok-1" },
    });
  });

  it("ステップ警告はトースト文へ付記される", () => {
    const outcome: FileOpOutcomeDto = {
      op_id: "1",
      kind: "move",
      status: "done",
      undoable: false,
      steps: [
        {
          file_id: "1",
          from: "/r/a.png",
          to: "/r/d/a.png",
          status: "done",
          resolution: null,
          error: null,
          warning: "跨界移動のコピー後の元ファイル削除に失敗しました",
        },
      ],
    };
    expect(outcomeNotice(outcome)).toContain("跨界移動のコピー後の元ファイル削除に失敗しました");
  });
});
