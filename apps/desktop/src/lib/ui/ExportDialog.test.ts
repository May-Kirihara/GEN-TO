// @vitest-environment happy-dom
/**
 * ExportDialog.svelte コンポーネントテスト (WP-14)。
 *
 * - オプションstate (構造toggle) とdry-run必須ゲーティング
 * - preview表の描画 (既存分類・caption由来)
 * - 実行→結果サマリ・失敗/衝突表示
 * - 実行中のキャンセルボタン (export_cancel呼び出し)
 * を実際の描画で検証する。IPCはモック化。
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const { invokeMock, listenMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
  listenMock: vi.fn(),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: listenMock.mockResolvedValue(() => {}),
}));

import { flushSync, mount, unmount } from "svelte";
import ExportDialog from "./ExportDialog.svelte";
import type { ExportPreviewDto, ExportResultDto } from "../export";
import type { LibraryRoot } from "../library";

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

const PREVIEW: ExportPreviewDto = {
  destDir: "/out",
  structure: "preserve",
  entries: [
    {
      assetId: "1",
      assetUuid: "u1",
      displayName: "img.png",
      sourcePath: "/photos/sub/img.png",
      outputRelPath: "sub/img.png",
      txtRelPath: "sub/img.txt",
      imageExisting: "none",
      txtExisting: "none",
      captionSource: "tags",
      captionPreview: "apple, blue sky",
      error: null,
    },
    {
      assetId: "2",
      assetUuid: "u2",
      displayName: "done.png",
      sourcePath: "/photos/done.png",
      outputRelPath: "done.png",
      txtRelPath: "done.txt",
      imageExisting: "identical",
      txtExisting: "identical",
      captionSource: "caption_sd_tags",
      captionPreview: "masterpiece",
      error: null,
    },
    {
      assetId: "3",
      assetUuid: "u3",
      displayName: "clash.png",
      sourcePath: "/photos/clash.png",
      outputRelPath: "clash.png",
      txtRelPath: "clash.txt",
      imageExisting: "conflict",
      txtExisting: "none",
      captionSource: "none",
      captionPreview: "",
      error: null,
    },
  ],
  summary: {
    total: 3,
    toWrite: 2,
    alreadyExported: 1,
    conflict: 1,
    error: 0,
  },
};

const RESULT: ExportResultDto = {
  manifestPath: "/out/export-manifest.json",
  cancelled: false,
  summary: {
    total: 3,
    exported: 1,
    identical: 1,
    conflict: 1,
    failed: 1,
    planningErrors: 0,
    cancelled: 0,
  },
  failures: [
    {
      assetUuid: "u9",
      displayName: "missing.png",
      outputRelPath: "missing.png",
      reason: "ソースファイルを読み取れません",
    },
  ],
  conflicts: [
    {
      assetUuid: "u3",
      displayName: "clash.png",
      outputRelPath: "clash.png",
      reason: "出力先に同名の異なる内容が既に存在します (画像)",
    },
  ],
  issuesTruncated: false,
};

let target: HTMLElement | null = null;
let component: Record<string, unknown> | null = null;

async function mountDialog(
  assetIds = ["1", "2", "3"],
): Promise<{ onClose: ReturnType<typeof vi.fn> }> {
  target = document.createElement("div");
  document.body.appendChild(target);
  const onClose = vi.fn();
  component = mount(ExportDialog, {
    target,
    props: {
      assetIds,
      sourceLabel: "グリッド選択",
      roots: [rootOf("/photos")],
      onClose,
    },
  });
  flushSync();
  return { onClose };
}

function click(selector: string): void {
  const el = target!.querySelector<HTMLElement>(selector);
  expect(el, selector).not.toBeNull();
  el!.click();
  flushSync();
}

function input(selector: string, value: string): void {
  const el = target!.querySelector<HTMLInputElement>(selector);
  expect(el, selector).not.toBeNull();
  el!.value = value;
  el!.dispatchEvent(new Event("input", { bubbles: true }));
  flushSync();
}

function textOf(selector: string): string {
  const el = target!.querySelector(selector);
  expect(el, selector).not.toBeNull();
  return el!.textContent ?? "";
}

async function proceedToPreview(preview: ExportPreviewDto = PREVIEW): Promise<void> {
  invokeMock.mockImplementation(async (command: string) => {
    if (command === "fs_list_dirs") {
      return { entries: [] };
    }
    if (command === "export_preview") {
      return preview;
    }
    throw new Error(`unexpected command: ${command}`);
  });
  input("[data-testid='export-dest']", "/out");
  click("[data-testid='export-preview-button']");
  await vi.waitFor(() => {
    expect(target!.querySelector("[data-testid='export-preview-table']")).not.toBeNull();
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
  listenMock.mockReset();
});

describe("ExportDialog (コンポーネント描画)", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "fs_list_dirs") {
        return { entries: [] };
      }
      throw new Error(`unexpected command: ${command}`);
    });
  });

  it("入力源と件数を表示し、dest未入力ではプレビュー/実行不可", async () => {
    await mountDialog();
    expect(textOf("[data-testid='export-source']")).toContain("グリッド選択");
    expect(textOf("[data-testid='export-source']")).toContain("3");
    const previewButton = target!.querySelector<HTMLButtonElement>(
      "[data-testid='export-preview-button']",
    )!;
    expect(previewButton.disabled).toBe(true);
    const executeButton = target!.querySelector<HTMLButtonElement>(
      "[data-testid='export-execute-button']",
    )!;
    expect(executeButton.disabled).toBe(true);
  });

  it("dry-runを実行し、表に既存分類とcaptionを表示する", async () => {
    await mountDialog();
    await proceedToPreview();
    // 集計行
    expect(textOf("[data-testid='export-preview-summary']")).toContain("書き出し予定: 2件");
    expect(textOf("[data-testid='export-preview-summary']")).toContain("衝突 (スキップ): 1件");
    // 表: 出力パス・既存ラベル・caption由来
    const table = textOf("[data-testid='export-preview-table']");
    expect(table).toContain("sub/img.png");
    expect(table).toContain("sub/img.txt");
    expect(table).toContain("新規");
    expect(table).toContain("書き出し済み");
    expect(table).toContain("衝突 (スキップ)");
    expect(table).toContain("タグ列");
    expect(table).toContain("apple, blue sky");
    expect(table).toContain("SDタグcaption");
    // previewの引数契約
    expect(invokeMock).toHaveBeenCalledWith("export_preview", {
      assetIds: ["1", "2", "3"],
      destDir: "/out",
      structure: "preserve",
    });
  });

  it("dry-run前に実行不可・dry-run後は実行可 (構造変更で再プレビュー要求)", async () => {
    await mountDialog();
    input("[data-testid='export-dest']", "/out");
    const executeButton = target!.querySelector<HTMLButtonElement>(
      "[data-testid='export-execute-button']",
    )!;
    expect(executeButton.disabled).toBe(true);
    await proceedToPreview();
    const executeAfter = target!.querySelector<HTMLButtonElement>(
      "[data-testid='export-execute-button']",
    )!;
    expect(executeAfter.disabled).toBe(false);
    // 構造をflatへ変更 → previewが無効化され実行不可に戻る
    const flatRadio = target!.querySelector<HTMLInputElement>(
      "input[name='export-structure'][value='flat']",
    )!;
    flatRadio.checked = true;
    flatRadio.dispatchEvent(new Event("change", { bubbles: true }));
    flushSync();
    const executeAfterFlip = target!.querySelector<HTMLButtonElement>(
      "[data-testid='export-execute-button']",
    )!;
    expect(executeAfterFlip.disabled).toBe(true);
    // 表も一旦消える (古いpreviewを表示しない)
    expect(target!.querySelector("[data-testid='export-preview-table']")).toBeNull();
  });

  it("実行すると結果サマリ・失敗・衝突・manifestパスを表示する", async () => {
    await mountDialog();
    await proceedToPreview();
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "export_execute") {
        return RESULT;
      }
      throw new Error(`unexpected command: ${command}`);
    });
    click("[data-testid='export-execute-button']");
    await vi.waitFor(() => {
      expect(target!.querySelector("[data-testid='export-done']")).not.toBeNull();
    });
    const summary = textOf("[data-testid='export-result-summary']");
    expect(summary).toContain("書き出し: 1件");
    expect(summary).toContain("失敗: 1件");
    expect(summary).toContain("衝突 (スキップ): 1件");
    expect(textOf("[data-testid='export-failures']")).toContain("missing.png");
    expect(textOf("[data-testid='export-failures']")).toContain("ソースファイルを読み取れません");
    expect(textOf("[data-testid='export-conflicts']")).toContain("clash.png");
    expect(target!.textContent).toContain("/out/export-manifest.json");
    expect(invokeMock).toHaveBeenCalledWith("export_execute", {
      assetIds: ["1", "2", "3"],
      destDir: "/out",
      structure: "preserve",
    });
  });

  it("実行中はキャンセルボタンが出て、クリックでexport_cancelを呼ぶ", async () => {
    await mountDialog();
    await proceedToPreview();
    let resolveExecute: (value: ExportResultDto) => void = () => {};
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "export_execute") {
        return new Promise<ExportResultDto>((resolve) => {
          resolveExecute = resolve;
        });
      }
      throw new Error(`unexpected command: ${command}`);
    });
    click("[data-testid='export-execute-button']");
    await vi.waitFor(() => {
      expect(target!.querySelector("[data-testid='export-cancel-button']")).not.toBeNull();
    });
    click("[data-testid='export-cancel-button']");
    await vi.waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("export_cancel");
    });
    resolveExecute(RESULT);
    await vi.waitFor(() => {
      expect(target!.querySelector("[data-testid='export-done']")).not.toBeNull();
    });
  });

  it("preview失敗はエラー表示し実行不可のまま", async () => {
    await mountDialog();
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "fs_list_dirs") {
        return { entries: [] };
      }
      if (command === "export_preview") {
        throw new Error("データベースが利用できません");
      }
      throw new Error(`unexpected command: ${command}`);
    });
    input("[data-testid='export-dest']", "/out");
    click("[data-testid='export-preview-button']");
    await vi.waitFor(() => {
      expect(textOf("[data-testid='export-error']")).toContain("データベースが利用できません");
    });
    const executeButton = target!.querySelector<HTMLButtonElement>(
      "[data-testid='export-execute-button']",
    )!;
    expect(executeButton.disabled).toBe(true);
  });
});
