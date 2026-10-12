// @vitest-environment happy-dom
import { afterEach, describe, expect, it } from "vitest";
import { originalUrl, previewUrl, thumbUrl } from "./protocol";

describe("thumbUrl", () => {
  it("s256 with version を localhost host 形式で生成する", () => {
    expect(thumbUrl("42", "s256", 3)).toBe("gento-thumb://localhost/asset/42/s256?v=3");
  });

  it("s512 も生成できる", () => {
    expect(thumbUrl("7", "s512", 1)).toBe("gento-thumb://localhost/asset/7/s512?v=1");
  });

  it("20桁のIDをそのまま通す", () => {
    const id = "12345678901234567890";
    expect(thumbUrl(id, "s256", 1)).toBe(`gento-thumb://localhost/asset/${id}/s256?v=1`);
  });

  it("version 0 は文法上有効なため許可する", () => {
    expect(thumbUrl("1", "s256", 0)).toBe("gento-thumb://localhost/asset/1/s256?v=0");
  });

  it("18桁のversionを許可する (Rust側上限と一致)", () => {
    // 注: 2^53超の整数はfloat64で正確に表せないため、
    // 正確に表現可能な 1e17 (18桁) を境界値に使う
    const gen = 1e17;
    expect(thumbUrl("1", "s256", gen)).toBe(
      `gento-thumb://localhost/asset/1/s256?v=${gen}`,
    );
  });

  it("不正なID (空・非数字・21桁) を拒否する", () => {
    expect(() => thumbUrl("", "s256", 1)).toThrow();
    expect(() => thumbUrl("12a", "s256", 1)).toThrow();
    expect(() => thumbUrl("-1", "s256", 1)).toThrow();
    expect(() => thumbUrl("+1", "s256", 1)).toThrow();
    expect(() => thumbUrl("1.0", "s256", 1)).toThrow();
    expect(() => thumbUrl("100000000000000000000", "s256", 1)).toThrow();
  });

  it("不正なversion (負・小数・19桁) を拒否する", () => {
    expect(() => thumbUrl("1", "s256", -1)).toThrow();
    expect(() => thumbUrl("1", "s256", 1.5)).toThrow();
    expect(() => thumbUrl("1", "s256", 1e17)).not.toThrow(); // 18桁
    expect(() => thumbUrl("1", "s256", 1e18)).toThrow(); // 19桁
  });

  it("thumbスキームで許可されないサイズクラスを拒否する", () => {
    // 型レベルでは除外されるが、実行時にも強制する
    expect(() => thumbUrl("1", "preview2048" as never, 1)).toThrow();
    expect(() => thumbUrl("1", "s1024" as never, 1)).toThrow();
  });
});

describe("previewUrl", () => {
  it("サイズクラスなしのURLを生成する", () => {
    expect(previewUrl("9", 2)).toBe("gento-preview://localhost/asset/9?v=2");
  });

  it("不正なID/versionを拒否する", () => {
    expect(() => previewUrl("x", 1)).toThrow();
    expect(() => previewUrl("1", Number.NaN)).toThrow();
  });
});

describe("originalUrl", () => {
  it("original scheme のURLを生成する", () => {
    expect(originalUrl("5", 1)).toBe("gento-original://localhost/asset/5?v=1");
  });

  it("不正なIDを拒否する", () => {
    expect(() => originalUrl("../etc/passwd", 1)).toThrow();
    expect(() => originalUrl("1%2F2", 1)).toThrow();
  });
});

/**
 * tauri (crates/tauri/scripts/core.js) が注入する `convertFileSrc` と同一挙動の
 * stub。本物はパス全体を `encodeURIComponent` で符号化する (区切りの `/` も
 * `%2F` になる) ため、それも含めて再現し、ビルダがプラットフォーム形式の基点に
 * 生の `/asset/...` パスを結合することを検証する
 * (Windows/WebView2 の回避形式 `http://{scheme}.localhost/{path}`)。
 */
type InternalsWindow = Window & {
  __TAURI_INTERNALS__?: {
    convertFileSrc: (filePath: string, protocol: string) => string;
  };
};

function stubTauriConvertFileSrc(windowsForm: boolean): void {
  (window as InternalsWindow).__TAURI_INTERNALS__ = {
    convertFileSrc: (filePath, protocol) => {
      const path = encodeURIComponent(filePath);
      return windowsForm
        ? `http://${protocol}.localhost/${path}`
        : `${protocol}://localhost/${path}`;
    },
  };
}

describe("プラットフォーム形式 (tauri convertFileSrc 経由)", () => {
  afterEach(() => {
    delete (window as InternalsWindow).__TAURI_INTERNALS__;
  });

  it("Windows/Android形式のinternalsでは http://{scheme}.localhost/ 形式を返す", () => {
    stubTauriConvertFileSrc(true);
    expect(thumbUrl("42", "s256", 3)).toBe(
      "http://gento-thumb.localhost/asset/42/s256?v=3",
    );
    expect(previewUrl("9", 2)).toBe("http://gento-preview.localhost/asset/9?v=2");
    expect(originalUrl("5", 1)).toBe("http://gento-original.localhost/asset/5?v=1");
  });

  it("非Windows形式のinternalsでは {scheme}://localhost/ 形式のまま", () => {
    stubTauriConvertFileSrc(false);
    expect(thumbUrl("42", "s256", 3)).toBe("gento-thumb://localhost/asset/42/s256?v=3");
    expect(originalUrl("5", 1)).toBe("gento-original://localhost/asset/5?v=1");
  });

  it("internalsが未注入なら従来形式へフォールバックする", () => {
    expect(thumbUrl("42", "s256", 3)).toBe("gento-thumb://localhost/asset/42/s256?v=3");
  });
});
