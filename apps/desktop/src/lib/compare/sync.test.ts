import { describe, expect, it } from "vitest";
import {
  MAX_SYNC_ZOOM_FACTOR,
  MIN_SYNC_ZOOM_FACTOR,
  initialSync,
  panSync,
  syncTransform,
  zoomSyncAt,
} from "./sync";
import { fitScale, imagePointAt } from "../viewer/transform";

const VP_A = { width: 800, height: 600 };
const DIMS_A = { width: 1600, height: 1000 }; // fit 0.5 (横制約)
const VP_B = { width: 800, height: 600 };
const DIMS_B = { width: 1000, height: 1000 }; // fit 0.6 (縦制約)

describe("syncTransform", () => {
  it("各スロットのscale = zoomFactor × slotFitScale、centerは共有値", () => {
    const sync = { centerX: 0.3, centerY: 0.7, zoomFactor: 1.8 };
    const a = syncTransform(sync, VP_A, DIMS_A);
    const b = syncTransform(sync, VP_B, DIMS_B);
    expect(a.scale).toBeCloseTo(1.8 * fitScale(VP_A, DIMS_A), 10);
    expect(b.scale).toBeCloseTo(1.8 * fitScale(VP_B, DIMS_B), 10);
    expect(a.centerX).toBeCloseTo(0.3, 10);
    expect(a.centerY).toBeCloseTo(0.7, 10);
    expect(b.centerX).toBeCloseTo(0.3, 10);
    expect(b.centerY).toBeCloseTo(0.7, 10);
  });

  it("zoomFactor=1はfit相当", () => {
    const a = syncTransform(initialSync(), VP_A, DIMS_A);
    expect(a.scale).toBeCloseTo(fitScale(VP_A, DIMS_A), 10);
    expect(a.mode).toBe("fit");
  });

  it("画像サイズ・アスペクトが異なっても同じ相対位置を比較できる", () => {
    const sync = { centerX: 0.25, centerY: 0.75, zoomFactor: 1 };
    const a = syncTransform(sync, VP_A, DIMS_A);
    const b = syncTransform(sync, VP_B, DIMS_B);
    // 両スロットとも「画像の正規化点 (0.25,0.75) がスロット中央」= 同相対位置
    const pa = imagePointAt(a, VP_A, DIMS_A, { x: VP_A.width / 2, y: VP_A.height / 2 });
    const pb = imagePointAt(b, VP_B, DIMS_B, { x: VP_B.width / 2, y: VP_B.height / 2 });
    expect(pa.u).toBeCloseTo(0.25, 9);
    expect(pa.v).toBeCloseTo(0.75, 9);
    expect(pb.u).toBeCloseTo(0.25, 9);
    expect(pb.v).toBeCloseTo(0.75, 9);
  });

  it("極端な共有中心は各スロットのマージン内へクランプされる (クランプ差は許容)", () => {
    const sync = { centerX: 50, centerY: -50, zoomFactor: 1 };
    const a = syncTransform(sync, VP_A, DIMS_A);
    // scaled 800x500 < vp → スライド可能範囲はマージン相当
    expect(a.centerX).toBeLessThanOrEqual(1 + (VP_A.width / 2 + 48) / (DIMS_A.width * a.scale) + 1e-9);
    expect(a.centerX).toBeGreaterThanOrEqual(-(VP_A.width / 2 + 48) / (DIMS_A.width * a.scale) - 1e-9);
  });
});

describe("zoomSyncAt", () => {
  it("操作スロット内のポインタ直下の画像座標がズーム前後で不変", () => {
    const sync = { centerX: 0.4, centerY: 0.6, zoomFactor: 1.2 };
    const pointer = { x: 300, y: 200 };
    const before = imagePointAt(syncTransform(sync, VP_A, DIMS_A), VP_A, DIMS_A, pointer);
    const zoomed = zoomSyncAt(sync, VP_A, DIMS_A, pointer, 1.6);
    const after = imagePointAt(syncTransform(zoomed, VP_A, DIMS_A), VP_A, DIMS_A, pointer);
    expect(after.u).toBeCloseTo(before.u, 9);
    expect(after.v).toBeCloseTo(before.v, 9);
  });

  it("zoomFactorは下限/上限にクランプされる", () => {
    const zoomed = zoomSyncAt(initialSync(), VP_A, DIMS_A, { x: 400, y: 300 }, 1e-6);
    expect(zoomed.zoomFactor).toBeCloseTo(MIN_SYNC_ZOOM_FACTOR, 9);
    const huge = zoomSyncAt(initialSync(), VP_A, DIMS_A, { x: 400, y: 300 }, 1e6);
    expect(huge.zoomFactor).toBeCloseTo(MAX_SYNC_ZOOM_FACTOR, 9);
  });
});

describe("panSync", () => {
  it("操作スロットのビューポートpxΔを正規化中心へ変換する", () => {
    const sync = initialSync();
    // slot A: scale = fit = 0.5 → scaledW=800。dx=200 → centerX -0.25
    const panned = panSync(sync, VP_A, DIMS_A, 200, -150);
    expect(panned.centerX).toBeCloseTo(0.5 - 200 / 800, 10);
    expect(panned.centerY).toBeCloseTo(0.5 + 150 / 500, 10);
  });

  it("共有中心は [0,1] にクランプされる", () => {
    const panned = panSync(initialSync(), VP_A, DIMS_A, -1e6, 1e6);
    expect(panned.centerX).toBeCloseTo(1, 10);
    expect(panned.centerY).toBeCloseTo(0, 10);
  });
});
