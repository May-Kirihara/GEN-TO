import { describe, expect, it } from "vitest";
import {
  SCROLL_SPEED_MAX,
  SCROLL_SPEED_MIN,
  canScrollWithin,
  coerceScrollSpeed,
  computeWheelScroll,
  resolveLineHeight,
} from "./wheel-speed";

describe("coerceScrollSpeed", () => {
  it("既定は1.0 (ネイティブ挙動)", () => {
    expect(coerceScrollSpeed(undefined)).toBe(1);
    expect(coerceScrollSpeed(null)).toBe(1);
    expect(coerceScrollSpeed(Number.NaN)).toBe(1);
    expect(coerceScrollSpeed("x")).toBe(1);
  });

  it("範囲クランプと0.1刻み丸め", () => {
    expect(coerceScrollSpeed(0.1)).toBe(SCROLL_SPEED_MIN);
    expect(coerceScrollSpeed(99)).toBe(SCROLL_SPEED_MAX);
    expect(coerceScrollSpeed(1.25000000001)).toBe(1.3);
    expect(coerceScrollSpeed(1.24999)).toBe(1.2);
    expect(coerceScrollSpeed(2)).toBe(2);
  });
});

describe("computeWheelScroll (deltaMode正規化・係数計算)", () => {
  // line高はCSS慣行値 (computed line-height ≒ 数十px以下)。
  // サムネ行extent (~200px) には依存しない (r1 B2)。
  const metrics = { lineHeight: 16, pageHeight: 800 };

  it("pixelモード (deltaMode=0) はdeltaYをそのまま係数倍する", () => {
    expect(computeWheelScroll(-100, 0, 2, metrics)).toBe(-200);
    expect(computeWheelScroll(-100, 0, 0.5, metrics)).toBe(-50);
  });

  it("lineモード (deltaMode=1) は行高さ換算する (3行×16px×2=96)", () => {
    expect(computeWheelScroll(-3, 1, 2, metrics)).toBe(-96);
  });

  it("pageモード (deltaMode=2) はviewport高さ換算する", () => {
    expect(computeWheelScroll(-0.5, 2, 2, metrics)).toBe(-800);
  });

  it("係数1.0は処理しない (リスナ未登録と等価)", () => {
    expect(computeWheelScroll(-100, 0, 1, metrics)).toBeNull();
  });

  it("ctrlKey (ピンチズーム) は処理しない", () => {
    expect(computeWheelScroll(-100, 0, 2, metrics, { ctrlKey: true })).toBeNull();
  });

  it("未知のdeltaModeはpixel扱い (前方互換)", () => {
    expect(computeWheelScroll(-100, 9, 2, metrics)).toBe(-200);
  });
});

describe("resolveLineHeight (lineモードの行高 — CSS慣行値)", () => {
  it("computed line-heightのpx値をそのまま使う", () => {
    expect(resolveLineHeight("16px")).toBe(16);
    expect(resolveLineHeight("22.4px")).toBe(22.4);
  });

  it("非px (normal等)・不正値・nullは慣例フォールバック16px", () => {
    expect(resolveLineHeight("normal")).toBe(16);
    expect(resolveLineHeight("1.5em")).toBe(16);
    expect(resolveLineHeight("")).toBe(16);
    expect(resolveLineHeight(null)).toBe(16);
    expect(resolveLineHeight(undefined)).toBe(16);
    expect(resolveLineHeight("0px")).toBe(16);
    expect(resolveLineHeight("-8px")).toBe(16);
    expect(resolveLineHeight("abc")).toBe(16);
  });
});

describe("canScrollWithin (境界チェイニング判定 — r1 B1)", () => {
  // clientHeight=400, scrollHeight=1000 → スクロール範囲 [0, 600]
  const bounds = { scrollTop: 300, clientHeight: 400, scrollHeight: 1000 };

  it("中間は両方向ともスクロール可能 (preventDefaultして適用)", () => {
    expect(canScrollWithin(-120, bounds)).toBe(true);
    expect(canScrollWithin(120, bounds)).toBe(true);
  });

  it("上端で上向きdeltaは不可 (親へのチェイニング)。下向きは可", () => {
    expect(canScrollWithin(-120, { ...bounds, scrollTop: 0 })).toBe(false);
    expect(canScrollWithin(120, { ...bounds, scrollTop: 0 })).toBe(true);
  });

  it("下端で下向きdeltaは不可 (親へのチェイニング)。上向きは可", () => {
    expect(canScrollWithin(120, { ...bounds, scrollTop: 600 })).toBe(false);
    expect(canScrollWithin(-120, { ...bounds, scrollTop: 600 })).toBe(true);
  });

  it("端未満の小数位置はさらに進める (クランプ相当の余裕がある間は適用)", () => {
    expect(canScrollWithin(120, { ...bounds, scrollTop: 599.5 })).toBe(true);
    expect(canScrollWithin(-120, { ...bounds, scrollTop: 0.5 })).toBe(true);
  });

  it("スクロール不能コンテナ (max<=0) は常に不可 (チェイニング)", () => {
    expect(canScrollWithin(120, { scrollTop: 0, clientHeight: 400, scrollHeight: 400 })).toBe(
      false,
    );
    expect(canScrollWithin(-120, { scrollTop: 0, clientHeight: 500, scrollHeight: 400 })).toBe(
      false,
    );
  });

  it("delta=0・非有限値は不可", () => {
    expect(canScrollWithin(0, bounds)).toBe(false);
    expect(canScrollWithin(Number.NaN, bounds)).toBe(false);
  });
});
