/**
 * 初回起動ガイドの純粋計算 (WP-18 E3)。
 *
 * 登録ルート0件のときだけ3ステップ (登録ルート / AI設定 / モデル配置) を
 * 導線として返す。ウィザードではなく空状態の充実として表示するため、
 * 表示条件 (グリッド空・検索/コレクション表示でない) はApp側で判定する。
 */

import { describe, expect, it } from "vitest";
import { deriveFirstRunGuide } from "./first-run";

describe("deriveFirstRunGuide", () => {
  it("登録ルート0件のとき3ステップを返す", () => {
    const steps = deriveFirstRunGuide({
      rootsCount: 0,
      aiProfilesCount: 0,
      model: { modelsDir: "/home/user/.local/share/gento-spike/models", verifiedCount: 0 },
    });
    expect(steps).not.toBeNull();
    expect(steps!.map((s) => s.key)).toEqual(["roots", "ai", "model"]);
    expect(steps![0].title).toContain("登録ルート");
    expect(steps![1].title).toContain("AI");
    expect(steps![2].title).toContain("モデル");
  });

  it("登録ルート1件以上ではnull (初回起動ではない)", () => {
    expect(
      deriveFirstRunGuide({
        rootsCount: 1,
        aiProfilesCount: 0,
        model: { modelsDir: null, verifiedCount: null },
      }),
    ).toBeNull();
  });

  it("AIステップ: プロファイル登録済みなら件数を、未登録なら未設定を示す (任意項目である旨)", () => {
    const steps = deriveFirstRunGuide({
      rootsCount: 0,
      aiProfilesCount: 2,
      model: { modelsDir: null, verifiedCount: null },
    });
    expect(steps!.find((s) => s.key === "ai")!.status).toContain("登録済み");
    expect(steps!.find((s) => s.key === "ai")!.status).toContain("2");

    const steps0 = deriveFirstRunGuide({
      rootsCount: 0,
      aiProfilesCount: 0,
      model: { modelsDir: null, verifiedCount: null },
    });
    expect(steps0!.find((s) => s.key === "ai")!.status).toContain("未設定");
  });

  it("モデルステップ: 配置先パス・配置状態 (配置済みn件/未配置/状態不明) を含む", () => {
    const placed = deriveFirstRunGuide({
      rootsCount: 0,
      aiProfilesCount: null,
      model: { modelsDir: "/data/models", verifiedCount: 2 },
    });
    const modelStep = placed!.find((s) => s.key === "model")!;
    expect(modelStep.body).toContain("/data/models");
    expect(modelStep.status).toContain("配置済み");
    expect(modelStep.status).toContain("2");

    const none = deriveFirstRunGuide({
      rootsCount: 0,
      aiProfilesCount: null,
      model: { modelsDir: "/data/models", verifiedCount: 0 },
    });
    expect(none!.find((s) => s.key === "model")!.status).toContain("未配置");

    const unknown = deriveFirstRunGuide({
      rootsCount: 0,
      aiProfilesCount: null,
      model: { modelsDir: null, verifiedCount: null },
    });
    expect(unknown!.find((s) => s.key === "model")!.status).toContain("状態不明");
  });

  it("AI設定なしでもファイラー機能は使用できる旨を明記する (MVP阻止条件の安心材料)", () => {
    const steps = deriveFirstRunGuide({
      rootsCount: 0,
      aiProfilesCount: 0,
      model: { modelsDir: null, verifiedCount: null },
    });
    expect(steps!.find((s) => s.key === "ai")!.body).toContain("使用できます");
    expect(steps!.find((s) => s.key === "model")!.body).toContain("--models-dir");
  });
});
