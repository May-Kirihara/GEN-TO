/**
 * バックアップと復元 (WP-13) の純粋関数テスト。
 *
 * バックエンド契約 (api-contract §47) に基づく plan集計・警告抽出・
 * 実行ゲーティング・結果集計・ディレクトリ名検証を検証する。
 */

import { describe, expect, it } from "vitest";

import {
  actionLabel,
  createBackupClient,
  looksLikeGentoBackup,
  matchLabel,
  planHasWork,
  planSummaryLines,
  planWarnings,
  resultSummaryLines,
  type RestorePlanDto,
  type RestoreResultDto,
} from "./backup";

function entry(
  matched: RestorePlanDto["entries"][number]["matched"],
  action: RestorePlanDto["entries"][number]["action"],
  displayName = "a.png",
  reason = "理由",
): RestorePlanDto["entries"][number] {
  return {
    assetUuid: `00000000-0000-7000-8000-00000000000${displayName.length % 10}`,
    contentHash: "blake3:cafe",
    displayName,
    currentAssetId: action === "skip" ? null : "1",
    matched,
    action,
    reason,
    files: [{ backupRelPath: displayName, currentRelPath: null, onDisk: false }],
  };
}

function planOf(
  entries: RestorePlanDto["entries"],
  collections: RestorePlanDto["collections"] = [],
): RestorePlanDto {
  const summary = {
    same: entries.filter((e) => e.action === "same").length,
    update: entries.filter((e) => e.action === "update").length,
    add: entries.filter((e) => e.action === "add").length,
    conflict: entries.filter((e) => e.matched === "conflict").length,
    missing: entries.filter((e) => e.matched === "missing").length,
    ambiguous: entries.filter((e) => e.matched === "ambiguous").length,
  };
  return {
    rootUuid: "u",
    rootDisplayPath: "/data/photos",
    createdAt: "2026-09-02T00:00:00.000Z",
    targetRootId: "1",
    targetRootPath: "/data/photos",
    entries,
    collections,
    summary,
    confirmToken: "tok",
  };
}

describe("matchLabel / actionLabel", () => {
  it("全分類の日本語ラベル", () => {
    expect(matchLabel("uuid")).toBe("UUID一致");
    expect(matchLabel("hash")).toBe("ハッシュ再関連付け");
    expect(matchLabel("path_add")).toBe("新規登録");
    expect(matchLabel("conflict")).toBe("衝突");
    expect(matchLabel("missing")).toBe("行方不明");
    expect(matchLabel("ambiguous")).toBe("曖昧 (同一内容複数箇所)");
    expect(actionLabel("same")).toBe("変更なし");
    expect(actionLabel("update")).toBe("更新");
    expect(actionLabel("add")).toBe("追加");
    expect(actionLabel("skip")).toBe("スキップ");
  });
});

describe("planSummaryLines", () => {
  it("件数0の行を返さない", () => {
    const lines = planSummaryLines(planOf([entry("uuid", "update")]));
    expect(lines).toEqual(["更新: 1件"]);
  });

  it("全分類とコレクションを行にする", () => {
    const plan = planOf(
      [
        entry("uuid", "same"),
        entry("hash", "update"),
        entry("path_add", "add"),
        entry("conflict", "skip", "c.png", "パス衝突"),
        entry("missing", "skip", "m.png", "不在"),
        entry("ambiguous", "skip", "x.png", "曖昧"),
      ],
      [
        {
          uuid: "c1",
          name: "新",
          kind: "manual",
          willCreate: true,
          itemsTotal: 2,
          itemsResolvable: 2,
        },
        {
          uuid: "c2",
          name: "旧",
          kind: "manual",
          willCreate: false,
          itemsTotal: 1,
          itemsResolvable: 1,
        },
      ],
    );
    expect(planSummaryLines(plan)).toEqual([
      "変更なし: 1件",
      "更新: 1件",
      "追加: 1件",
      "衝突 (スキップ): 1件",
      "行方不明 (スキップ): 1件",
      "曖昧 (スキップ): 1件",
      "コレクション新規作成: 1件",
      "コレクション項目マージ: 1件",
    ]);
  });
});

describe("planWarnings", () => {
  it("衝突・行方不明・曖昧と未解決コレクション項目を抽出", () => {
    const plan = planOf(
      [
        entry("uuid", "same", "ok.png"),
        entry("conflict", "skip", "c.png", "パスに別内容"),
        entry("missing", "skip", "m.png", "ファイル不在"),
      ],
      [
        {
          uuid: "c1",
          name: "部分的",
          kind: "manual",
          willCreate: true,
          itemsTotal: 3,
          itemsResolvable: 1,
        },
      ],
    );
    const warnings = planWarnings(plan);
    expect(warnings).toHaveLength(3);
    expect(warnings[0]).toContain("c.png");
    expect(warnings[0]).toContain("パスに別内容");
    expect(warnings[1]).toContain("m.png");
    expect(warnings[2]).toBe("コレクション「部分的」: 2件の項目が復元対象に解決できません");
  });

  it("問題が無ければ空", () => {
    expect(planWarnings(planOf([entry("uuid", "same")]))).toEqual([]);
  });
});

describe("planHasWork (確認フローのゲーティング)", () => {
  it("更新/追加があれば実行可能", () => {
    expect(planHasWork(planOf([entry("uuid", "update")]))).toBe(true);
    expect(planHasWork(planOf([entry("path_add", "add")]))).toBe(true);
  });

  it("全部スキップ + コレクション無しは実行不可", () => {
    expect(
      planHasWork(planOf([entry("conflict", "skip"), entry("missing", "skip")])),
    ).toBe(false);
  });

  it("解決可能なコレクション項目があれば実行可能", () => {
    const plan = planOf([], [
      {
        uuid: "c1",
        name: "新",
        kind: "manual",
        willCreate: true,
        itemsTotal: 2,
        itemsResolvable: 2,
      },
    ]);
    expect(planHasWork(plan)).toBe(true);
  });

  it("smartコレクションの新規作成は項目0でも実行可能", () => {
    const plan = planOf([], [
      {
        uuid: "c1",
        name: "高評価",
        kind: "smart",
        willCreate: true,
        itemsTotal: 0,
        itemsResolvable: 0,
      },
    ]);
    expect(planHasWork(plan)).toBe(true);
  });
});

describe("resultSummaryLines", () => {
  it("実行結果の集計行", () => {
    const result: RestoreResultDto = {
      applied: [
        { assetUuid: "a", action: "add", changed: ["created"] },
        { assetUuid: "b", action: "update", changed: ["rating"] },
        { assetUuid: "c", action: "same", changed: [] },
      ],
      skipped: [{ assetUuid: "d", matched: "missing", reason: "不在" }],
      collectionsCreated: 1,
      collectionsMerged: 0,
      collectionItemsAdded: 2,
      collectionItemsUnresolved: 1,
      ftsRebuilt: 2,
    };
    expect(resultSummaryLines(result)).toEqual([
      "追加: 1件",
      "更新: 1件",
      "変更なし: 1件",
      "スキップ: 1件",
      "コレクション作成: 1件",
      "コレクション項目追加: 2件",
      "コレクション未解決項目: 1件",
      "FTS再構築: 2件",
    ]);
  });
});

describe("looksLikeGentoBackup", () => {
  it(".gentoで終わる通常名を受け入れる", () => {
    expect(looksLikeGentoBackup("photos-20260902T101010000Z.gento")).toBe(true);
  });
  it("tmp残骸と拡張子なしを拒否", () => {
    expect(looksLikeGentoBackup(".gento-tmp-abc.gento")).toBe(false);
    expect(looksLikeGentoBackup("photos")).toBe(false);
    expect(looksLikeGentoBackup(".gento")).toBe(false);
  });
});

describe("createBackupClient (IPCラップ)", () => {
  it("snake_case引数でcommandを呼ぶ", async () => {
    const calls: Array<{ command: string; args?: Record<string, unknown> }> = [];
    const client = createBackupClient({
      invoke: async <T>(command: string, args?: Record<string, unknown>) => {
        calls.push({ command, args });
        return { ok: true } as T;
      },
    });
    await client.createPortable("3", "/tmp/exports");
    await client.fullDb(null);
    await client.defaultDir();
    await client.restorePlan("/tmp/b.gento", "3");
    await client.restoreExecute("/tmp/b.gento", "tok", null);
    await client.settings();
    await client.setSettings("daily", 5);
    expect(calls.map((c) => c.command)).toEqual([
      "backup_portable_create",
      "backup_full_db",
      "backup_default_dir",
      "backup_restore_plan",
      "backup_restore_execute",
      "backup_settings_get",
      "backup_settings_set",
    ]);
    expect(calls[0].args).toEqual({ rootId: "3", destDir: "/tmp/exports" });
    expect(calls[3].args).toEqual({ backupDir: "/tmp/b.gento", targetRootId: "3" });
    expect(calls[4].args).toEqual({
      backupDir: "/tmp/b.gento",
      confirmToken: "tok",
      targetRootId: null,
    });
    expect(calls[6].args).toEqual({ autoFullDb: "daily", retentionGenerations: 5 });
  });
});
