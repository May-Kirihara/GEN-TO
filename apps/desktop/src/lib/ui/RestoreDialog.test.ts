// @vitest-environment happy-dom
/**
 * RestoreDialog.svelte コンポーネントテスト (FG-WP13-1)。
 *
 * レビュー指摘「planの全分類 (衝突/行方不明/曖昧) が確認前にユーザーに
 * 見えること」「確認なしでは実行できないこと」を実際の描画で検証する。
 * IPC (@tauri-apps/api/core invoke) はモック化し、表示ロジックの正本
 * (backup.ts純粋関数) を通じた実描画結果をアサートする。
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const { invokeMock } = vi.hoisted(() => ({ invokeMock: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));

import { flushSync, mount, unmount } from "svelte";
import RestoreDialog from "./RestoreDialog.svelte";
import type { LibraryRoot } from "../library";
import type { RestoreEntryDto, RestorePlanDto, RestoreResultDto } from "../backup";

const BACKUP_ROOT = "/backups/photos-20260902T101010000Z.gento";

function rootOf(path: string): LibraryRoot {
  return {
    id: "1",
    uuid: "00000000-0000-7000-8000-000000000001",
    path,
    enabled: true,
    watch_enabled: false,
    available: true,
    ai_enabled: true,
    send_exif: false,
    send_gps: false,
  };
}

function entry(
  matched: RestoreEntryDto["matched"],
  action: RestoreEntryDto["action"],
  displayName: string,
): RestoreEntryDto {
  // 表示名から決定論的なUUID (テスト内識別のみ。衝突しなければ十分)
  let hash = 0;
  for (const ch of displayName) {
    hash = (hash * 31 + (ch.codePointAt(0) ?? 0)) | 0;
  }
  const suffix = Math.abs(hash).toString().padStart(12, "0").slice(0, 12);
  return {
    assetUuid: `00000000-0000-7000-8000-${suffix}`,
    contentHash: `blake3:${displayName}`,
    displayName,
    currentAssetId: action === "skip" ? null : "1",
    matched,
    action,
    reason: `${displayName}の理由`,
    files: [{ backupRelPath: displayName, currentRelPath: null, onDisk: false }],
  };
}

function planOf(entries: RestoreEntryDto[], confirmToken = "tok-123"): RestorePlanDto {
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
    collections: [],
    summary,
    confirmToken,
  };
}

const RESULT_DTO: RestoreResultDto = {
  applied: [
    { assetUuid: "a", action: "add", changed: ["created"] },
    { assetUuid: "b", action: "update", changed: ["rating"] },
  ],
  skipped: [{ assetUuid: "c", matched: "conflict", reason: "パスに別内容" }],
  collectionsCreated: 0,
  collectionsMerged: 0,
  collectionItemsAdded: 0,
  collectionItemsUnresolved: 0,
  ftsRebuilt: 2,
};

let target: HTMLElement | null = null;
let component: Record<string, unknown> | null = null;

async function mountDialog(): Promise<{ onClose: ReturnType<typeof vi.fn> }> {
  target = document.createElement("div");
  document.body.appendChild(target);
  const onClose = vi.fn();
  // ルート一覧の最終成分が .gento ならバックアップ候補として選択可能
  const roots = [rootOf("/data/photos"), rootOf(BACKUP_ROOT)];
  component = mount(RestoreDialog, { target, props: { roots, onClose } });
  flushSync();
  return { onClose };
}

function click(selector: string): void {
  const el = target!.querySelector<HTMLElement>(selector);
  expect(el, selector).not.toBeNull();
  el!.click();
  flushSync();
}

/** ルート選択 → 「復元内容を確認」でplanを取得するまで進める。 */
async function proceedToPlan(plan: RestorePlanDto): Promise<void> {
  invokeMock.mockImplementation(async (command: string) => {
    if (command === "backup_restore_plan") {
      return plan;
    }
    if (command === "backup_restore_execute") {
      return RESULT_DTO;
    }
    throw new Error(`unexpected command: ${command}`);
  });
  click(".node-gento");
  await vi.waitFor(() => {
    const selected = target!.querySelector<HTMLElement>("[data-testid='restore-selected']");
    expect(selected?.textContent).toContain(BACKUP_ROOT);
  });
  click("[data-testid='restore-plan']");
  await vi.waitFor(() => {
    expect(target!.querySelector("[data-testid='restore-plan-summary']")).not.toBeNull();
  });
}

afterEach(() => {
  if (component) {
    unmount(component);
    component = null;
  }
  target?.remove();
  target = null;
  invokeMock.mockReset();
});

describe("RestoreDialog (コンポーネント描画)", () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it("バックアップ選択前は実行への導線が存在しない (確認なしには実行できない)", async () => {
    await mountDialog();
    // 初期画面: .gento選択ツリーのみ。plan確認ボタンは選択まで無効。
    expect(target!.querySelector("[data-testid='restore-execute']")).toBeNull();
    const planButton = target!.querySelector<HTMLButtonElement>(
      "[data-testid='restore-plan']",
    );
    expect(planButton?.disabled).toBe(true);
  });

  it("plan表示に全分類 (変更なし/更新/追加/衝突/行方不明/曖昧) が現れる", async () => {
    await mountDialog();
    const plan = planOf([
      entry("uuid", "same", "same.png"),
      entry("hash", "update", "moved.png"),
      entry("path_add", "add", "new.png"),
      entry("conflict", "skip", "clash.png"),
      entry("missing", "skip", "gone.png"),
      entry("ambiguous", "skip", "dup.png"),
    ]);
    await proceedToPlan(plan);

    // サマリ行に6分類が全て出る
    const summary = target!.querySelector("[data-testid='restore-plan-summary']")!;
    const text = summary.textContent ?? "";
    for (const label of [
      "変更なし: 1件",
      "更新: 1件",
      "追加: 1件",
      "衝突 (スキップ): 1件",
      "行方不明 (スキップ): 1件",
      "曖昧 (スキップ): 1件",
    ]) {
      expect(text).toContain(label);
    }

    // 警告領域: 衝突・行方不明・曖昧の内訳が確認前に見える
    const warnings = target!.querySelector("[data-testid='restore-plan-warnings']")!;
    const warningText = warnings.textContent ?? "";
    expect(warningText).toContain("clash.png");
    expect(warningText).toContain("clash.pngの理由");
    expect(warningText).toContain("gone.png");
    expect(warningText).toContain("dup.png");

    // 項目ごとの内訳: 6分類のmatchedラベルが全て描画される
    const entryList = target!.querySelector(".entry-list")!;
    const entryText = entryList.textContent ?? "";
    for (const label of [
      "UUID一致",
      "ハッシュ再関連付け",
      "新規登録",
      "衝突",
      "行方不明",
      "曖昧 (同一内容複数箇所)",
    ]) {
      expect(entryText).toContain(label);
    }
  });

  it("作業があるplanでは実行ボタンが有効 — 実行は確認トークンを渡す", async () => {
    await mountDialog();
    const plan = planOf([entry("hash", "update", "moved.png")]);
    await proceedToPlan(plan);
    const execute = target!.querySelector<HTMLButtonElement>(
      "[data-testid='restore-execute']",
    )!;
    expect(execute.disabled).toBe(false);
    click("[data-testid='restore-execute']");
    await vi.waitFor(() => {
      expect(target!.querySelector("[data-testid='restore-result']")).not.toBeNull();
    });
    const call = invokeMock.mock.calls.find(([command]) => command === "backup_restore_execute");
    expect(call?.[1]).toEqual({
      backupDir: BACKUP_ROOT,
      confirmToken: "tok-123",
      targetRootId: null,
    });
    // 結果レポート: 適用・スキップが表示される
    const resultText =
      target!.querySelector("[data-testid='restore-result']")?.textContent ?? "";
    expect(resultText).toContain("更新: 1件");
    expect(resultText).toContain("スキップ: 1件");
  });

  it("全項目スキップのplanでは実行ボタンが無効 (適用する物が無い)", async () => {
    await mountDialog();
    const plan = planOf([
      entry("conflict", "skip", "clash.png"),
      entry("missing", "skip", "gone.png"),
      entry("ambiguous", "skip", "dup.png"),
    ]);
    await proceedToPlan(plan);
    const execute = target!.querySelector<HTMLButtonElement>(
      "[data-testid='restore-execute']",
    )!;
    expect(execute.disabled).toBe(true);
    // 実行commandは呼ばれない
    click("[data-testid='restore-execute']");
    expect(
      invokeMock.mock.calls.some(([command]) => command === "backup_restore_execute"),
    ).toBe(false);
  });

  it("plan取得がエラーならエラー表示して実行へ進めない", async () => {
    await mountDialog();
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "backup_restore_plan") {
        throw "バックアップ /tmp/x.gento の内容検証に失敗しました";
      }
      throw new Error(`unexpected command: ${command}`);
    });
    click(".node-gento");
    await vi.waitFor(() => {
      expect(target!.querySelector<HTMLElement>("[data-testid='restore-selected']")?.textContent)
        .toContain(BACKUP_ROOT);
    });
    click("[data-testid='restore-plan']");
    await vi.waitFor(() => {
      const errorText =
        target!.querySelector<HTMLElement>("[data-testid='restore-error']")?.textContent;
      expect(errorText).toContain("内容検証に失敗しました");
    });
    expect(target!.querySelector("[data-testid='restore-execute']")).toBeNull();
  });
});
