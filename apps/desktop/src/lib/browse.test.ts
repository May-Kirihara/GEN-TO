import { describe, expect, it } from "vitest";
import {
  breadcrumbOf,
  deriveEmptyState,
  formatTimestamp,
  normalizePastedPath,
} from "./browse";

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

describe("normalizePastedPath (UIUX-1: パンくずパス入力)", () => {
  it("前後空白をtrimし絶対パスをそのまま通す", () => {
    expect(normalizePastedPath("  /home/user/pictures ")).toEqual({
      ok: true,
      path: "/home/user/pictures",
    });
  });

  it("末尾スラッシュを正規化 (/ 自体は保存)", () => {
    expect(normalizePastedPath("/home/user/")).toEqual({ ok: true, path: "/home/user" });
    expect(normalizePastedPath("/home/user//")).toEqual({ ok: true, path: "/home/user" });
    expect(normalizePastedPath("/")).toEqual({ ok: true, path: "/" });
  });

  it("対になった引用符 (二重・単一) のみ除去する", () => {
    expect(normalizePastedPath('"/home/user/pic"')).toEqual({
      ok: true,
      path: "/home/user/pic",
    });
    expect(normalizePastedPath("'/home/user/pic'")).toEqual({
      ok: true,
      path: "/home/user/pic",
    });
    // 片側だけ・対でない引用符はパスの一部として保持 (保守的)
    expect(normalizePastedPath('/home/u"ser')).toEqual({ ok: true, path: '/home/u"ser' });
    expect(normalizePastedPath('"home')).toEqual({ ok: false, error: expect.any(String) });
  });

  it("改行・NULを拒否する", () => {
    expect(normalizePastedPath("/home/u\nser").ok).toBe(false);
    expect(normalizePastedPath("/home/u\rser").ok).toBe(false);
    expect(normalizePastedPath("/home/u\x00ser").ok).toBe(false);
  });

  it("file:// URIは非対応として通知 (percent-decodeしない)", () => {
    const result = normalizePastedPath("file:///home/user/pic");
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error).toContain("file://");
    }
    // 通常パスの%はdecodeしない
    expect(normalizePastedPath("/home/a%20b")).toEqual({ ok: true, path: "/home/a%20b" });
  });

  it("相対パス・空は拒否 (~展開もしない)", () => {
    expect(normalizePastedPath("pictures").ok).toBe(false);
    expect(normalizePastedPath("./pictures").ok).toBe(false);
    expect(normalizePastedPath("").ok).toBe(false);
    expect(normalizePastedPath("   ").ok).toBe(false);
    expect(normalizePastedPath("~/pictures").ok).toBe(false);
  });

  it("Windowsドライブ・UNCパスを受け入れ、ドライブルートの末尾は保存する", () => {
    expect(normalizePastedPath("C:\\Users\\me\\pic")).toEqual({
      ok: true,
      path: "C:\\Users\\me\\pic",
    });
    // ドライブルート (C:\) の末尾区切りは保存
    expect(normalizePastedPath("C:\\")).toEqual({ ok: true, path: "C:\\" });
    expect(normalizePastedPath("C:/Users/me/pic/")).toEqual({
      ok: true,
      path: "C:/Users/me/pic",
    });
    expect(normalizePastedPath("\\\\server\\share\\pic")).toEqual({
      ok: true,
      path: "\\\\server\\share\\pic",
    });
  });

  it("..や.セグメントは字句判断せずbackendへ委譲する", () => {
    expect(normalizePastedPath("/home/user/../other")).toEqual({
      ok: true,
      path: "/home/user/../other",
    });
  });

  it("http等のURLスキームはfile://と同じ通知経路で拒否される", () => {
    expect(normalizePastedPath("https://example.com/x").ok).toBe(false);
  });
});
