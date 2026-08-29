/**
 * 比較表示の同期状態 (WP-6 / フェーズ7 §14「同期規則」)。
 *
 * - 共有するのは画像内の正規化中心座標 (centerX/Y) と
 *   「fitに対する倍率」zoomFactor (アスペクト比が異なるスロット間で
 *   同じ相対位置・相対倍率を比較するため)
 * - 各スロットの実際のtransformは slotTransform で導出 (スロット毎クランプ)
 * - ズーム・パンの入力変換は「操作したスロット」の幾何で行う
 *   (ポインタ直下の画像座標不変は操作スロットで成立。他スロットは同相対位置へ追従)
 */
import {
  PAN_MARGIN_PX,
  clampTransform,
  fitScale,
  zoomAtPoint,
  type Dims,
  type Viewport,
  type ViewTransform,
} from "../viewer/transform";

export type SyncState = {
  centerX: number;
  centerY: number;
  /** fitに対する倍率。1 = 各スロットのfit。 */
  zoomFactor: number;
};

export const MIN_SYNC_ZOOM_FACTOR = 0.05;
export const MAX_SYNC_ZOOM_FACTOR = 20;

export function initialSync(): SyncState {
  return { centerX: 0.5, centerY: 0.5, zoomFactor: 1 };
}

function clampCenter01(value: number): number {
  return Math.min(1, Math.max(0, value));
}

function clampZoomFactor(value: number): number {
  return Math.min(MAX_SYNC_ZOOM_FACTOR, Math.max(MIN_SYNC_ZOOM_FACTOR, value));
}

/** 共有同期状態からスロットの有効transformを導出する (スロット毎クランプ付き)。 */
export function syncTransform(sync: SyncState, viewport: Viewport, dims: Dims): ViewTransform {
  const scale = sync.zoomFactor * fitScale(viewport, dims);
  return clampTransform(
    {
      scale,
      centerX: sync.centerX,
      centerY: sync.centerY,
      mode: sync.zoomFactor === 1 ? "fit" : "custom",
    },
    viewport,
    dims,
  );
}

/** 操作スロットのポインタ位置を中心としたズーム。 */
export function zoomSyncAt(
  sync: SyncState,
  viewport: Viewport,
  dims: Dims,
  pointer: { x: number; y: number },
  factor: number,
): SyncState {
  const current = syncTransform(sync, viewport, dims);
  const zoomed = zoomAtPoint(current, viewport, dims, pointer, factor);
  return {
    centerX: clampCenter01(zoomed.centerX),
    centerY: clampCenter01(zoomed.centerY),
    zoomFactor: clampZoomFactor(sync.zoomFactor * factor),
  };
}

/** 操作スロットのビューポートpxΔによるパン。 */
export function panSync(
  sync: SyncState,
  viewport: Viewport,
  dims: Dims,
  dx: number,
  dy: number,
): SyncState {
  const scale = sync.zoomFactor * fitScale(viewport, dims);
  const scaledW = dims.width * scale;
  const scaledH = dims.height * scale;
  return {
    centerX: clampCenter01(scaledW > 0 ? sync.centerX - dx / scaledW : sync.centerX),
    centerY: clampCenter01(scaledH > 0 ? sync.centerY - dy / scaledH : sync.centerY),
    zoomFactor: sync.zoomFactor,
  };
}

/** パンマージン (スロット毎クランプの既定値を共有)。 */
export const SYNC_PAN_MARGIN_PX = PAN_MARGIN_PX;
