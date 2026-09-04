import { describe, expect, it } from "vitest";
import {
  SCROLL_SPEED_MAX,
  SCROLL_SPEED_MIN,
  coerceScrollSpeed,
  computeWheelScroll,
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
  const metrics = { lineHeight: 200, pageHeight: 800 };

  it("pixelモード (deltaMode=0) はdeltaYをそのまま係数倍する", () => {
    expect(computeWheelScroll(-100, 0, 2, metrics)).toBe(-200);
    expect(computeWheelScroll(-100, 0, 0.5, metrics)).toBe(-50);
  });

  it("lineモード (deltaMode=1) は行高さ換算する", () => {
    expect(computeWheelScroll(-3, 1, 2, metrics)).toBe(-1200);
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
