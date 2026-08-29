import { describe, expect, it } from "vitest";
import { originalUrl, previewUrl, thumbUrl } from "./protocol";

describe("thumbUrl", () => {
  it("s256 with generation を localhost host 形式で生成する", () => {
    expect(thumbUrl("42", "s256", 3)).toBe("gento-thumb://localhost/asset/42/s256?v=3");
  });

  it("s512 も生成できる", () => {
    expect(thumbUrl("7", "s512", 1)).toBe("gento-thumb://localhost/asset/7/s512?v=1");
  });

  it("20桁のIDをそのまま通す", () => {
    const id = "12345678901234567890";
    expect(thumbUrl(id, "s256", 1)).toBe(`gento-thumb://localhost/asset/${id}/s256?v=1`);
  });

  it("generation 0 は文法上有効なため許可する", () => {
    expect(thumbUrl("1", "s256", 0)).toBe("gento-thumb://localhost/asset/1/s256?v=0");
  });

  it("18桁のgenerationを許可する (Rust側上限と一致)", () => {
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

  it("不正なgeneration (負・小数・19桁) を拒否する", () => {
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

  it("不正なID/generationを拒否する", () => {
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
