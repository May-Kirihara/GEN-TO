import { describe, expect, it } from "vitest";
import { breadcrumbOf, deriveEmptyState, formatTimestamp } from "./browse";

describe("breadcrumbOf", () => {
  it("絶対パスを / から順に分割する", () => {
    expect(breadcrumbOf("/home/u/pictures")).toEqual([
      { name: "/", path: "/" },
      { name: "home", path: "/home" },
      { name: "u", path: "/home/u" },
      { name: "pictures", path: "/home/u/pictures" },
    ]);
  });

  it("ルート単体・空文字・末尾スラッシュ", () => {
    expect(breadcrumbOf("/")).toEqual([{ name: "/", path: "/" }]);
    expect(breadcrumbOf("")).toEqual([]);
    expect(breadcrumbOf("/a/")).toEqual([
      { name: "/", path: "/" },
      { name: "a", path: "/a" },
    ]);
  });
});

describe("deriveEmptyState", () => {
  it("項目があれば空状態ではない", () => {
    expect(
      deriveEmptyState({ scanning: true, total: 10, folderError: null }),
    ).toBeNull();
  });

  it("フォルダエラーが最優先", () => {
    const state = deriveEmptyState({
      scanning: false,
      total: 0,
      folderError: "フォルダが見つかりません: /x",
    });
    expect(state?.kind).toBe("folder-error");
    expect(state?.detail).toContain("見つかりません");
  });

  it("スキャン中は読み込み中表示", () => {
    expect(deriveEmptyState({ scanning: true, total: 0, folderError: null })?.kind).toBe(
      "scanning",
    );
  });

  it("完了後の空は「画像が見つからない」(未対応形式を含む)", () => {
    const state = deriveEmptyState({ scanning: false, total: 0, folderError: null });
    expect(state?.kind).toBe("empty");
    expect(state?.message).toContain("画像が見つかりません");
  });
});

describe("formatTimestamp", () => {
  it("有効値はロケール文字列、無効値は—", () => {
    expect(formatTimestamp(0)).toBe("—");
    expect(formatTimestamp(Number.NaN)).toBe("—");
    const text = formatTimestamp(Date.UTC(2026, 7, 30, 1, 2, 3));
    expect(text).toContain("2026");
  });
});
