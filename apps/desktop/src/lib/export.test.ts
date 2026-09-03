/**
 * SD学習用エクスポート (WP-14) の純粋関数テスト。
 *
 * 命名規則 (txtNameFor / collisionSuffixName) はbackend (Rust export.rsの
 * txt_file_name / suffixed_name) と同じ期待値を固定するミラー。
 * 集計・ゲーティングの表示ロジックの正本はここにある。
 */

import { describe, expect, it } from "vitest";

import {
  canExecute,
  captionSourceLabel,
  collisionSuffixName,
  createExportClient,
  existingLabel,
  previewSummaryLines,
  resultSummaryLines,
  structureLabel,
  txtNameFor,
  type ExportPreviewDto,
  type ExportResultDto,
} from "./export";

function previewOf(
  overrides: Partial<ExportPreviewDto["summary"]> = {},
): ExportPreviewDto {
  return {
    destDir: "/out",
    structure: "flat",
    entries: [],
    summary: {
      total: 3,
      toWrite: 2,
      alreadyExported: 1,
      conflict: 0,
      error: 0,
      ...overrides,
    },
  };
}

const RESULT: ExportResultDto = {
  manifestPath: "/out/export-manifest.json",
  cancelled: false,
  summary: {
    total: 3,
    exported: 2,
    identical: 1,
    conflict: 0,
    failed: 0,
    planningErrors: 0,
    cancelled: 0,
  },
  failures: [],
  conflicts: [],
  issuesTruncated: false,
};

describe("export 命名ミラー (backend契約)", () => {
  it("txtNameFor: stem + .txt (拡張子なしにも付ける)", () => {
    expect(txtNameFor("img.png")).toBe("img.txt");
    expect(txtNameFor("archive.tar.gz")).toBe("archive.tar.txt");
    expect(txtNameFor("README")).toBe("README.txt");
    // 先頭dotのみの名前はstem扱い (Rust file_stemと同一)
    expect(txtNameFor(".hidden")).toBe(".hidden.txt");
  });

  it("collisionSuffixName: {stem}-{n}.{ext}", () => {
    expect(collisionSuffixName("img.png", 1)).toBe("img-1.png");
    expect(collisionSuffixName("a.b.png", 2)).toBe("a.b-2.png");
    expect(collisionSuffixName("README", 3)).toBe("README-3");
  });
});

describe("export 表示ラベル", () => {
  it("構造・既存関係・caption由来の日本語ラベル", () => {
    expect(structureLabel("preserve")).toBe("フォルダ構造を維持");
    expect(structureLabel("flat")).toBe("平坦化 (直下へ)");
    expect(existingLabel("none")).toBe("新規");
    expect(existingLabel("identical")).toBe("書き出し済み");
    expect(existingLabel("conflict")).toBe("衝突 (スキップ)");
    expect(existingLabel("n/a")).toBe("—");
    expect(captionSourceLabel("caption_sd_tags")).toBe("SDタグcaption");
    expect(captionSourceLabel("caption_description")).toBe("説明caption");
    expect(captionSourceLabel("tags")).toBe("タグ列");
    expect(captionSourceLabel("none")).toBe("(空)");
  });
});

describe("export 集計表示", () => {
  it("previewSummaryLines: 件数0の行を返さない", () => {
    const lines = previewSummaryLines(previewOf({}));
    expect(lines).toEqual(["書き出し予定: 2件", "書き出し済み (スキップ): 1件"]);
  });

  it("previewSummaryLines: 全部0なら空配列", () => {
    expect(
      previewSummaryLines(
        previewOf({ toWrite: 0, alreadyExported: 0, conflict: 0, error: 0 }),
      ),
    ).toEqual([]);
  });

  it("resultSummaryLines: 失敗・計画エラー・キャンセルも件数に応じて出す", () => {
    const lines = resultSummaryLines({
      ...RESULT,
      cancelled: true,
      summary: {
        ...RESULT.summary,
        exported: 1,
        identical: 0,
        conflict: 1,
        failed: 1,
        planningErrors: 1,
        cancelled: 2,
      },
    });
    expect(lines).toEqual([
      "書き出し: 1件",
      "衝突 (スキップ): 1件",
      "失敗: 1件",
      "計画エラー: 1件",
      "キャンセル未処理: 2件",
    ]);
  });
});

describe("canExecute (dry-runゲーティング)", () => {
  it("dest未入力・preview無し・全件エラーでは実行不可", () => {
    expect(canExecute("", previewOf())).toBe(false);
    expect(canExecute("  ", previewOf())).toBe(false);
    expect(canExecute("/out", null)).toBe(false);
    expect(
      canExecute(
        "/out",
        previewOf({ toWrite: 0, alreadyExported: 0, error: 3 }),
      ),
    ).toBe(false);
  });

  it("dest入力済み + 書き出し/確認対象ありなら実行可", () => {
    expect(canExecute("/out", previewOf())).toBe(true);
    // 全件書き出し済み (冪等再実行の確認) も実行可
    expect(
      canExecute("/out", previewOf({ toWrite: 0, alreadyExported: 3 })),
    ).toBe(true);
  });
});

describe("createExportClient", () => {
  it("command名・引数を契約どおりに渡す", async () => {
    const calls: Array<{ command: string; args?: Record<string, unknown> }> = [];
    const client = createExportClient({
      invoke: async <T>(command: string, args?: Record<string, unknown>) => {
        calls.push({ command, args });
        if (command === "export_preview") {
          return previewOf({}) as T;
        }
        if (command === "export_execute") {
          return RESULT as T;
        }
        return undefined as T;
      },
    });
    await client.preview(["1", "2"], "/out", "preserve");
    await client.execute(["1", "2"], "/out", "preserve");
    await client.cancel();
    expect(calls).toEqual([
      {
        command: "export_preview",
        args: { assetIds: ["1", "2"], destDir: "/out", structure: "preserve" },
      },
      {
        command: "export_execute",
        args: { assetIds: ["1", "2"], destDir: "/out", structure: "preserve" },
      },
      { command: "export_cancel" },
    ]);
  });
});
