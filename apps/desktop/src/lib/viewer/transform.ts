/**
 * ビューア・比較表示共通の ViewTransform 純粋計算 (WP-6 / 本体計画 §13状態案)。
 *
 * - centerX/centerY は画像内正規化座標 (0..1)。比較表示でスロット間共有する
 * - scale は「画像px → CSSpx」。actual = 1:1
 * - CSS適用: 画像要素をビューポート中央に配置し、
 *   translate(tx,ty) scale(k) を乘せる。tx = (0.5 - center) * dims * scale
 *   このとき画面上の画像点 p の位置は viewportCenter + (p - center) * dims * scale
 *   となり、centerX/Y が常に画面中心に来る画像点を表す
 */

export type Dims = { width: number; height: number };
export type Viewport = { width: number; height: number };
export type TransformMode = "fit" | "actual" | "custom";

export type ViewTransform = {
  scale: number;
  centerX: number;
  centerY: number;
  mode: TransformMode;
};

/** パンで画像端がビューポート外へ出られる余白 (px)。 */
export const PAN_MARGIN_PX = 48;
/** ズーム倍率のfit相対下限/上限。 */
export const MIN_ZOOM_FACTOR = 0.05;
export const MAX_ZOOM_FACTOR = 20;

export function fitScale(viewport: Viewport, dims: Dims): number {
  if (dims.width <= 0 || dims.height <= 0 || viewport.width <= 0 || viewport.height <= 0) {
    return 1;
  }
  return Math.min(viewport.width / dims.width, viewport.height / dims.height);
}

export function fitTransform(viewport: Viewport, dims: Dims): ViewTransform {
  return { scale: fitScale(viewport, dims), centerX: 0.5, centerY: 0.5, mode: "fit" };
}

/** fit相対倍率 (§14 同期規則: 基本倍率は fit に対する倍率として共有)。 */
export function zoomFactorOf(transform: ViewTransform, viewport: Viewport, dims: Dims): number {
  const fit = fitScale(viewport, dims);
  return fit > 0 ? transform.scale / fit : transform.scale;
}

/**
 * 中心のクランプ。画像端がビューポート端からマージンを超えて外へ出ない範囲に制限する
 * (アスペクト比が異なるスロットでも画像外へ過度に移動させない)。
 * 画像がビューポートより小さい場合も同じ規則でスライドを許す
 * (マウス位置中心ズームの座標不変性をfit未満の倍率でも保つため)。
 */
export function clampCenter(
  viewport: Viewport,
  dims: Dims,
  scale: number,
  centerX: number,
  centerY: number,
  margin = PAN_MARGIN_PX,
): { centerX: number; centerY: number } {
  const clampAxis = (vp: number, dim: number, center: number): number => {
    const scaled = dim * scale;
    if (scaled <= 0) {
      return 0.5;
    }
    const limit = (vp / 2 + margin) / scaled;
    return Math.min(1 + limit, Math.max(-limit, center));
  };
  return {
    centerX: clampAxis(viewport.width, dims.width, centerX),
    centerY: clampAxis(viewport.height, dims.height, centerY),
  };
}

export function clampTransform(
  transform: ViewTransform,
  viewport: Viewport,
  dims: Dims,
  margin = PAN_MARGIN_PX,
): ViewTransform {
  const { centerX, centerY } = clampCenter(
    viewport,
    dims,
    transform.scale,
    transform.centerX,
    transform.centerY,
    margin,
  );
  return { ...transform, centerX, centerY };
}

/** CSS transform: translate(tx,ty) scale(k) への変換 (要素は中央配置前提)。 */
export function toCss(transform: ViewTransform, dims: Dims): { tx: number; ty: number; scale: number } {
  return {
    tx: (0.5 - transform.centerX) * dims.width * transform.scale,
    ty: (0.5 - transform.centerY) * dims.height * transform.scale,
    scale: transform.scale,
  };
}

/** 画像正規化座標 (u,v) の画面上の位置 (ビューポート左上基準px)。 */
export function screenPointOf(
  transform: ViewTransform,
  viewport: Viewport,
  dims: Dims,
  point: { u: number; v: number },
): { x: number; y: number } {
  return {
    x: viewport.width / 2 + (point.u - transform.centerX) * dims.width * transform.scale,
    y: viewport.height / 2 + (point.v - transform.centerY) * dims.height * transform.scale,
  };
}

/** 画面上の位置 (ビューポート左上基準px) の画像正規化座標。 */
export function imagePointAt(
  transform: ViewTransform,
  viewport: Viewport,
  dims: Dims,
  pointer: { x: number; y: number },
): { u: number; v: number } {
  const scaledW = dims.width * transform.scale;
  const scaledH = dims.height * transform.scale;
  return {
    u: scaledW > 0 ? transform.centerX + (pointer.x - viewport.width / 2) / scaledW : 0.5,
    v: scaledH > 0 ? transform.centerY + (pointer.y - viewport.height / 2) / scaledH : 0.5,
  };
}

/**
 * マウス位置中心のズーム。ポインタ直下の画像正規化座標が
 * ズーム前後で不変になるよう中心を更新する。
 */
export function zoomAtPoint(
  transform: ViewTransform,
  viewport: Viewport,
  dims: Dims,
  pointer: { x: number; y: number },
  factor: number,
  margin = PAN_MARGIN_PX,
): ViewTransform {
  const fit = fitScale(viewport, dims);
  const minScale = MIN_ZOOM_FACTOR * fit;
  const maxScale = MAX_ZOOM_FACTOR * fit;
  const nextScale = Math.min(maxScale, Math.max(minScale, transform.scale * factor));
  const point = imagePointAt(transform, viewport, dims, pointer);
  const raw: ViewTransform = {
    scale: nextScale,
    centerX: point.u - (pointer.x - viewport.width / 2) / (dims.width * nextScale || 1),
    centerY: point.v - (pointer.y - viewport.height / 2) / (dims.height * nextScale || 1),
    mode: "custom",
  };
  return clampTransform(raw, viewport, dims, margin);
}

/** ドラッグΔ (ビューポートpx) によるパン。画像をΔ方向へ動かす。 */
export function panBy(
  transform: ViewTransform,
  viewport: Viewport,
  dims: Dims,
  dx: number,
  dy: number,
  margin = PAN_MARGIN_PX,
): ViewTransform {
  const scaledW = dims.width * transform.scale;
  const scaledH = dims.height * transform.scale;
  const raw: ViewTransform = {
    ...transform,
    centerX: scaledW > 0 ? transform.centerX - dx / scaledW : transform.centerX,
    centerY: scaledH > 0 ? transform.centerY - dy / scaledH : transform.centerY,
    mode: "custom",
  };
  return clampTransform(raw, viewport, dims, margin);
}

/** モード切替。fit=全リセット、actual=1:1 (中心はクランプのみ)、custom=現状維持。 */
export function withMode(
  transform: ViewTransform,
  mode: TransformMode,
  viewport: Viewport,
  dims: Dims,
): ViewTransform {
  if (mode === "fit") {
    return fitTransform(viewport, dims);
  }
  if (mode === "actual") {
    return clampTransform({ ...transform, scale: 1, mode: "actual" }, viewport, dims);
  }
  return clampTransform(transform, viewport, dims);
}
