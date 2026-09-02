/**
 * embedding.ts純粋ヘルパーの単体テスト (WP-11)。
 * バックエンドDTO契約 (camelCase) のフロント側ミラーも固定する。
 */
import { describe, expect, it } from "vitest";
import {
  MODEL_CHOICES,
  canEnable,
  enableConfirmText,
  modelLabel,
  packageStateLabel,
  progressRatio,
  progressText,
  workerPhaseLabel,
  type EmbeddingStatus,
} from "./embedding";

function status(overrides: Partial<EmbeddingStatus> = {}): EmbeddingStatus {
  return {
    enabled: false,
    model: "ruri-v3-70m",
    modelsDir: "/data/models",
    packages: [
      {
        modelId: "ruri-v3-70m",
        state: "verified",
        detail: "検証済み",
        packageDir: "/data/models/ruri-v3-70m",
        embeddingDim: 384,
        license: "Apache-2.0",
      },
      {
        modelId: "ruri-v3-30m",
        state: "missing",
        detail: "未インストール",
        packageDir: "/data/models/ruri-v3-30m",
        embeddingDim: 256,
        license: "Apache-2.0",
      },
    ],
    worker: { phase: "not_started", restartCount: 0, lastError: null },
    loadedModel: null,
    embeddedCount: 0,
    totalCount: 100,
    pendingJobs: 0,
    selfcheckMargin: null,
    warning: null,
    ...overrides,
  };
}

describe("packageStateLabel", () => {
  it("全状態を日本語ラベルへ", () => {
    expect(packageStateLabel("verified")).toBe("検証済み");
    expect(packageStateLabel("missing")).toBe("未インストール");
    expect(packageStateLabel("missing_files")).toBe("ファイル欠落");
    expect(packageStateLabel("mismatch")).toBe("検証失敗 (破損)");
    expect(packageStateLabel("weird")).toBe("weird");
  });
});

describe("workerPhaseLabel", () => {
  it("全フェーズを日本語ラベルへ", () => {
    expect(workerPhaseLabel("not_started")).toBe("未起動");
    expect(workerPhaseLabel("running")).toBe("実行中");
    expect(workerPhaseLabel("restarting")).toBe("再起動中");
    expect(workerPhaseLabel("crashed")).toContain("自動再起停止");
  });
});

describe("canEnable", () => {
  it("選択モデルが検証済みなら有効化可能", () => {
    expect(canEnable(status())).toBe(true);
  });

  it("選択モデル未インストールなら不可", () => {
    expect(canEnable(status({ model: "ruri-v3-30m" }))).toBe(false);
  });

  it("ワーカークラッシュ停止中は不可 (手動復帰が必要)", () => {
    expect(
      canEnable(
        status({ worker: { phase: "crashed", restartCount: 5, lastError: "x" } }),
      ),
    ).toBe(false);
  });

  it("検証失敗パッケージは不可", () => {
    const broken = status();
    broken.packages[0] = { ...broken.packages[0], state: "mismatch" };
    expect(canEnable(broken)).toBe(false);
  });
});

describe("enableConfirmText", () => {
  it("対象件数とローカル実行であることを提示", () => {
    const text = enableConfirmText(status({ totalCount: 12345 }));
    expect(text).toContain("12,345件");
    expect(text).toContain("ローカル");
    expect(text).toContain("標準 (70m)");
  });
});

describe("modelLabel", () => {
  it("選択肢ラベルと未知IDのフォールバック", () => {
    expect(modelLabel("ruri-v3-70m")).toBe("標準 (70m)");
    expect(modelLabel("ruri-v3-30m")).toBe("軽量 (30m)");
    expect(modelLabel("custom")).toBe("custom");
  });

  it("選択肢は70m/30mの2つ", () => {
    expect(MODEL_CHOICES.map((c) => c.id)).toEqual(["ruri-v3-70m", "ruri-v3-30m"]);
  });
});

describe("progress", () => {
  it("比率の計算 (0件時は1)", () => {
    expect(progressRatio(status({ embeddedCount: 0, totalCount: 100 }))).toBe(0);
    expect(progressRatio(status({ embeddedCount: 50, totalCount: 100 }))).toBe(0.5);
    expect(progressRatio(status({ embeddedCount: 100, totalCount: 100 }))).toBe(1);
    expect(progressRatio(status({ embeddedCount: 999, totalCount: 0 }))).toBe(1);
  });

  it("無効時と有効時のテキスト", () => {
    expect(progressText(status())).toBe("無効");
    const text = progressText(
      status({ enabled: true, embeddedCount: 3, totalCount: 10, pendingJobs: 7 }),
    );
    expect(text).toContain("3 / 10");
    expect(text).toContain("7");
  });
});

describe("DTO契約 (camelCaseミラー)", () => {
  it("バックエンド応答のフィールド名と一致", () => {
    // バックエンド EmbeddingStatusDto の serde camelCase 契約:
    // enabled, model, modelsDir, packages[].modelId/... , worker.phase,
    // loadedModel, embeddedCount, totalCount, pendingJobs, selfcheckMargin, warning
    const dto = status({
      enabled: true,
      loadedModel: "ruri-v3-70m",
      selfcheckMargin: 0.12,
      warning: null,
    });
    expect(dto.modelsDir).toBe("/data/models");
    expect(dto.packages[0].modelId).toBe("ruri-v3-70m");
    expect(dto.packages[0].embeddingDim).toBe(384);
    expect(dto.worker.phase).toBe("not_started");
    expect(dto.worker.restartCount).toBe(0);
    expect(dto.loadedModel).toBe("ruri-v3-70m");
    expect(dto.embeddedCount).toBe(0);
    expect(dto.totalCount).toBe(100);
    expect(dto.pendingJobs).toBe(0);
    expect(dto.selfcheckMargin).toBe(0.12);
  });
});
