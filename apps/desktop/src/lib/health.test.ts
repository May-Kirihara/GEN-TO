import { describe, expect, it } from "vitest";
import { normalizeHealth } from "./health";

describe("normalizeHealth", () => {
  it("正常応答 (データセットあり・DB開) をok状態へ正規化する", () => {
    const result = normalizeHealth({
      app_name: "GEN-TO 技術検証プロトタイプ",
      version: "0.1.0",
      api_version: 1,
      dataset_path: "/home/user/GEN-TO/sample/dataset",
      dataset_exists: true,
      db_open: true,
      timestamp_ms: 1_759_000_000_000,
    });

    expect(result).toEqual({
      kind: "ok",
      datasetPresent: true,
      datasetPath: "/home/user/GEN-TO/sample/dataset",
      dbOpen: true,
      timestampMs: 1_759_000_000_000,
    });
  });

  it("データセット不在応答をok + datasetPresent=falseへ正規化する", () => {
    const result = normalizeHealth({
      app_name: "GEN-TO 技術検証プロトタイプ",
      version: "0.1.0",
      api_version: 1,
      dataset_path: "/home/user/GEN-TO/sample/dataset",
      dataset_exists: false,
      db_open: true,
      timestamp_ms: 1_759_000_000_001,
    });

    expect(result).toEqual({
      kind: "ok",
      datasetPresent: false,
      datasetPath: "/home/user/GEN-TO/sample/dataset",
      dbOpen: true,
      timestampMs: 1_759_000_000_001,
    });
  });

  it("DB未開 (db_open=false) 応答をok + dbOpen=falseへ正規化する (WP-1)", () => {
    const result = normalizeHealth({
      app_name: "GEN-TO 技術検証プロトタイプ",
      version: "0.1.0",
      api_version: 1,
      dataset_path: null,
      dataset_exists: false,
      db_open: false,
      timestamp_ms: 1,
    });

    expect(result).toEqual({
      kind: "ok",
      datasetPresent: false,
      datasetPath: null,
      dbOpen: false,
      timestampMs: 1,
    });
  });

  it("データセット未設定 (null) 応答をok + datasetPath=nullへ正規化する", () => {
    const result = normalizeHealth({
      app_name: "GEN-TO 技術検証プロトタイプ",
      version: "0.1.0",
      api_version: 1,
      dataset_path: null,
      dataset_exists: false,
      db_open: true,
      timestamp_ms: 1,
    });

    expect(result).toEqual({
      kind: "ok",
      datasetPresent: false,
      datasetPath: null,
      dbOpen: true,
      timestampMs: 1,
    });
  });

  it("未知の追加フィールドを持つ応答はokとして許容する (前方互換)", () => {
    const result = normalizeHealth({
      app_name: "GEN-TO 技術検証プロトタイプ",
      version: "0.1.0",
      api_version: 1,
      dataset_path: null,
      dataset_exists: false,
      db_open: true,
      timestamp_ms: 1,
      future_field: "ignored",
    });

    expect(result).toEqual({
      kind: "ok",
      datasetPresent: false,
      datasetPath: null,
      dbOpen: true,
      timestampMs: 1,
    });
  });

  it("api_version欠如・型違反はerrorへ落とす (WP-0の必須フィールド)", () => {
    const base = {
      app_name: "GEN-TO 技術検証プロトタイプ",
      version: "0.1.0",
      dataset_path: null,
      dataset_exists: false,
      db_open: true,
      timestamp_ms: 1,
    };
    for (const broken of [{ ...base }, { ...base, api_version: "1" }]) {
      const result = normalizeHealth(broken);
      expect(result.kind).toBe("error");
    }
  });

  it("db_open欠如・型違反はerrorへ落とす (WP-1の必須フィールド)", () => {
    const base = {
      app_name: "GEN-TO 技術検証プロトタイプ",
      version: "0.1.0",
      api_version: 1,
      dataset_path: null,
      dataset_exists: false,
      timestamp_ms: 1,
    };
    // WP-1より前の形状 (db_openなし) と、型違反
    for (const broken of [{ ...base }, { ...base, db_open: "true" }]) {
      const result = normalizeHealth(broken);
      expect(result.kind).toBe("error");
    }
  });

  it("非オブジェクト応答をerror状態へ正規化する", () => {
    for (const invalid of [null, undefined, 42, "ok", []]) {
      const result = normalizeHealth(invalid);
      expect(result.kind).toBe("error");
    }
  });

  it("形状が不正な応答をerror状態へ正規化する", () => {
    const base = {
      app_name: "GEN-TO 技術検証プロトタイプ",
      version: "0.1.0",
      api_version: 1,
      dataset_path: null,
      dataset_exists: false,
      db_open: true,
      timestamp_ms: 1,
    };

    const brokenVariants: unknown[] = [
      {},
      { ...base, app_name: 123 },
      { ...base, version: null },
      { ...base, dataset_path: 42 },
      { ...base, dataset_exists: "true" },
      { ...base, db_open: 1 },
      { ...base, timestamp_ms: "1" },
    ];

    for (const broken of brokenVariants) {
      const result = normalizeHealth(broken);
      expect(result.kind).toBe("error");
    }
  });
});
