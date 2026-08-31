import { describe, expect, it } from "vitest";
import { parseTriageKey, triageActionLabel } from "./triage-keys";

describe("parseTriageKey", () => {
  it("数字1-5は星評価へ変換する", () => {
    for (let n = 1; n <= 5; n++) {
      expect(parseTriageKey(String(n))).toEqual({ kind: "rating", value: n });
    }
  });

  it("0は評価解除 (0星)", () => {
    expect(parseTriageKey("0")).toEqual({ kind: "rating", value: 0 });
  });

  it("Xは除外 (大文字小文字どちらも)", () => {
    expect(parseTriageKey("x")).toEqual({ kind: "cull", value: "rejected" });
    expect(parseTriageKey("X")).toEqual({ kind: "cull", value: "rejected" });
  });

  it("PとAは採用 (大文字小文字どちらも)", () => {
    for (const key of ["p", "P", "a", "A"]) {
      expect(parseTriageKey(key)).toEqual({ kind: "cull", value: "picked" });
    }
  });

  it("該当なしのキーはnull (ズーム等の既存キーは素通し)", () => {
    for (const key of ["+", "-", "r", "f", "ArrowLeft", "Enter", " ", "6", "9", ""]) {
      expect(parseTriageKey(key)).toBeNull();
    }
  });

  it("修飾キー付きは対象外 (OS/ブラウザ操作と衝突させない)", () => {
    expect(parseTriageKey("3", { ctrlKey: true })).toBeNull();
    expect(parseTriageKey("x", { metaKey: true })).toBeNull();
    expect(parseTriageKey("p", { altKey: true })).toBeNull();
  });
});

describe("triageActionLabel", () => {
  it("操作の短いラベル", () => {
    expect(triageActionLabel({ kind: "rating", value: 3 })).toBe("評価 ★3");
    expect(triageActionLabel({ kind: "rating", value: 0 })).toBe("評価解除");
    expect(triageActionLabel({ kind: "cull", value: "picked" })).toBe("採用");
    expect(triageActionLabel({ kind: "cull", value: "rejected" })).toBe("除外");
  });
});
