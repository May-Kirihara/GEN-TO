import { describe, expect, it } from "vitest";
import {
  MAX_ZOOM_FACTOR,
  MIN_ZOOM_FACTOR,
  clampTransform,
  fitScale,
  fitTransform,
  imagePointAt,
  panBy,
  screenPointOf,
  toCss,
  withMode,
  zoomAtPoint,
  zoomFactorOf,
  type ViewTransform,
} from "./transform";

const VP = { width: 800, height: 600 };

function t(scale: number, cx: number, cy: number, mode: ViewTransform["mode"] = "custom"): ViewTransform {
  return { scale, centerX: cx, centerY: cy, mode };
}

describe("fitScale", () => {
  it("縦横どちらか狭い方に合わせる", () => {
    // 横長画像 (横が制約: 800/1600 < 600/1000)
    expect(fitScale(VP, { width: 1600, height: 1000 })).toBeCloseTo(0.5, 10);
    // 縦長画像 (縦が制約: 600/1600 < 800/1000)
    expect(fitScale(VP, { width: 1000, height: 1600 })).toBeCloseTo(0.375, 10);
  });

  it("極端なアスペクト比でも破綻しない", () => {
    expect(fitScale(VP, { width: 20000, height: 10 })).toBeCloseTo(0.04, 10);
    expect(fitScale(VP, { width: 10, height: 20000 })).toBeCloseTo(0.03, 10);
  });

  it("ビューアより小さい画像は拡大してフィットする", () => {
    expect(fitScale(VP, { width: 400, height: 200 })).toBeCloseTo(2, 10);
  });

  it("無効な寸法・ビューポートは1", () => {
    expect(fitScale(VP, { width: 0, height: 100 })).toBe(1);
    expect(fitScale({ width: 0, height: 0 }, { width: 100, height: 100 })).toBe(1);
  });
});

describe("toCss / screenPointOf / imagePointAt", () => {
  it("center 0.5/0.5 はビューポート中央に画像中心を置く", () => {
    const dims = { width: 1600, height: 1000 };
    const view = fitTransform(VP, dims); // scale 0.5
    const css = toCss(view, dims);
    const center = screenPointOf(view, VP, dims, { u: 0.5, v: 0.5 });
    expect(center.x).toBeCloseTo(VP.width / 2, 6);
    expect(center.y).toBeCloseTo(VP.height / 2, 6);
    // 要素は中央配置を前提に translate(tx,ty) scale(k) を適用する
    expect(css.tx).toBeCloseTo(0, 6);
    expect(css.ty).toBeCloseTo(0, 6);
    expect(css.scale).toBeCloseTo(0.5, 10);
  });

  it("center変更は translate = (0.5 - center) * dims * scale", () => {
    const dims = { width: 1000, height: 500 };
    const view = t(2, 0.25, 0.75);
    const css = toCss(view, dims);
    expect(css.tx).toBeCloseTo((0.5 - 0.25) * 1000 * 2, 10);
    expect(css.ty).toBeCloseTo((0.5 - 0.75) * 500 * 2, 10);
    // 画像中心 (0.25,0.75) が画面中央へ来る
    const center = screenPointOf(view, VP, dims, { u: 0.25, v: 0.75 });
    expect(center.x).toBeCloseTo(VP.width / 2, 6);
    expect(center.y).toBeCloseTo(VP.height / 2, 6);
  });

  it("imagePointAt は screenPointOf の逆変換", () => {
    const dims = { width: 1200, height: 900 };
    const view = t(1.7, 0.3, 0.8);
    const p = { x: 355, y: 420 };
    const img = imagePointAt(view, VP, dims, p);
    const back = screenPointOf(view, VP, dims, img);
    expect(back.x).toBeCloseTo(p.x, 6);
    expect(back.y).toBeCloseTo(p.y, 6);
  });
});

describe("zoomAtPoint (マウス位置中心ズーム)", () => {
  const dims = { width: 1600, height: 1000 };

  it("ポインタ直下の画像座標がズーム前後で不変", () => {
    const view = fitTransform(VP, dims);
    const pointer = { x: 300, y: 200 };
    const before = imagePointAt(view, VP, dims, pointer);
    // fit未満へズームアウトすると画像がビューポートに収まり中央固定が優先されるため
    // 不変性が成立するのは拡大側 (クランプ不発の範囲)
    for (const factor of [1.05, 1.5, 2.7]) {
      const zoomed = zoomAtPoint(view, VP, dims, pointer, factor);
      const after = imagePointAt(zoomed, VP, dims, pointer);
      expect(after.u).toBeCloseTo(before.u, 9);
      expect(after.v).toBeCloseTo(before.v, 9);
    }
  });

  it("fit未満へのズームアウトでもマージンクランプ内では不変性が保たれる", () => {
    const view = fitTransform(VP, dims);
    const pointer = { x: 300, y: 200 };
    const before = imagePointAt(view, VP, dims, pointer);
    const zoomed = zoomAtPoint(view, VP, dims, pointer, 0.5);
    const after = imagePointAt(zoomed, VP, dims, pointer);
    expect(after.u).toBeCloseTo(before.u, 9);
    expect(after.v).toBeCloseTo(before.v, 9);
  });

  it("パン済み状態でも不変 (クランプされない範囲)", () => {
    const view = clampTransform(panBy(t(1, 0.5, 0.5), VP, dims, -200, 100), VP, dims);
    const pointer = { x: 600, y: 450 };
    const before = imagePointAt(view, VP, dims, pointer);
    const zoomed = zoomAtPoint(view, VP, dims, pointer, 1.8);
    const after = imagePointAt(zoomed, VP, dims, pointer);
    expect(after.u).toBeCloseTo(before.u, 9);
    expect(after.v).toBeCloseTo(before.v, 9);
  });

  it("倍率はfit相対の下限/上限にクランプされる", () => {
    const view = fitTransform(VP, dims);
    const pointer = { x: 400, y: 300 };
    const tiny = zoomAtPoint(view, VP, dims, pointer, 1e-6);
    expect(zoomFactorOf(tiny, VP, dims)).toBeCloseTo(MIN_ZOOM_FACTOR, 9);
    const huge = zoomAtPoint(view, VP, dims, pointer, 1e6);
    expect(zoomFactorOf(huge, VP, dims)).toBeCloseTo(MAX_ZOOM_FACTOR, 9);
  });

  it("ズームするとmodeはcustomになる", () => {
    const view = fitTransform(VP, dims);
    expect(zoomAtPoint(view, VP, dims, { x: 400, y: 300 }, 1.5).mode).toBe("custom");
  });
});

describe("panBy とクランプ", () => {
  it("ドラッグΔ分だけ中心が移動する (画像をドラッグ方向へ動かす)", () => {
    const dims = { width: 1600, height: 1000 };
    const view = t(1, 0.5, 0.5);
    const panned = panBy(view, VP, dims, 160, -100);
    expect(panned.centerX).toBeCloseTo(0.5 - 160 / 1600, 10);
    expect(panned.centerY).toBeCloseTo(0.5 + 100 / 1000, 10);
    // 画像中心はドラッグΔだけ動く
    const before = screenPointOf(view, VP, dims, { u: 0.5, v: 0.5 });
    const after = screenPointOf(clampTransform(panned, VP, dims), VP, dims, { u: 0.5, v: 0.5 });
    expect(after.x).toBeCloseTo(before.x + 160, 6);
    expect(after.y).toBeCloseTo(before.y - 100, 6);
  });

  it("画像端がマージンを超えて外へ移動しないようクランプする", () => {
    const dims = { width: 2000, height: 1000 };
    const view = t(1, 0.5, 0.5);
    const panned = clampTransform(panBy(view, VP, dims, -1e6, 1e6), VP, dims);
    // 画像左端 = vp/2 - cx*scaled ≤ vp + margin、右端 ≥ -margin
    const left = VP.width / 2 - panned.centerX * 2000;
    const right = VP.width / 2 + (1 - panned.centerX) * 2000;
    expect(left).toBeLessThanOrEqual(VP.width + 48 + 1e-6);
    expect(right).toBeGreaterThanOrEqual(-48 - 1e-6);
  });

  it("ビューポートより小さい画像もマージンを超えて外へは移動できない", () => {
    const dims = { width: 400, height: 200 };
    const view = fitTransform(VP, dims); // scaled 800x400
    const panned = clampTransform(panBy(view, VP, dims, 500, 500), VP, dims);
    // 画像端が vp + 48 / -48 を超えない
    const left = VP.width / 2 - panned.centerX * 800;
    const right = left + 800;
    expect(left).toBeLessThanOrEqual(VP.width + 48 + 1e-6);
    expect(right).toBeGreaterThanOrEqual(-48 - 1e-6);
    // 中央 (0.5) は常に許容範囲
    const centered = clampTransform({ ...view, centerX: 0.5, centerY: 0.5 }, VP, dims);
    expect(centered.centerX).toBeCloseTo(0.5, 10);
  });

  it("マージンは既定48px、変更可能", () => {
    const dims = { width: 2000, height: 1000 };
    // panByは既定マージンでクランプするため、生の過剰中心値に直接適用して検証
    const loose = clampTransform(t(1, -499.5, 0.5), VP, dims, 200);
    const left = VP.width / 2 - loose.centerX * 2000;
    expect(left).toBeCloseTo(VP.width + 200, 6);
  });
});

describe("mode往復", () => {
  const dims = { width: 1600, height: 1000 };

  it("fit → custom → fit でfit状態に復元される", () => {
    const fit = fitTransform(VP, dims);
    const custom = clampTransform(panBy({ ...fit, mode: "custom" }, VP, dims, 120, 80), VP, dims);
    expect(custom.mode).toBe("custom");
    const back = withMode(custom, "fit", VP, dims);
    expect(back).toEqual(fit);
  });

  it("fit → actual → fit", () => {
    const fit = fitTransform(VP, dims);
    const actual = withMode(fit, "actual", VP, dims);
    expect(actual.scale).toBeCloseTo(1, 10);
    expect(actual.mode).toBe("actual");
    expect(withMode(actual, "fit", VP, dims)).toEqual(fit);
  });

  it("actualは画像px:CSSpx = 1:1、中心はクランプのみ", () => {
    const custom = t(0.6, 0.9, 0.1);
    const actual = withMode(custom, "actual", VP, dims);
    expect(actual.scale).toBeCloseTo(1, 10);
    expect(actual.centerX).toBeCloseTo(0.9, 10);
    expect(actual.centerY).toBeCloseTo(0.1, 10);
  });

  it("custom維持のwithModeは値を変えない", () => {
    const custom = t(1.3, 0.4, 0.6);
    expect(withMode(custom, "custom", VP, dims)).toEqual(custom);
  });
});

describe("zoomFactorOf (fit相対倍率)", () => {
  it("fit時は1、actual時は1/fit", () => {
    const dims = { width: 1600, height: 1000 };
    const fit = fitTransform(VP, dims); // fit=0.5
    expect(zoomFactorOf(fit, VP, dims)).toBeCloseTo(1, 10);
    expect(zoomFactorOf(withMode(fit, "actual", VP, dims), VP, dims)).toBeCloseTo(2, 10);
  });
});
