/**
 * UIUX-3 V-4: 単一ビューアの平常ホイール=画像送りのジェスチャ解釈 (純粋)。
 *
 * デザイン根拠 (計画書「改訂」§1 — accumulator+閾値+gesture cooldown/latch):
 * - トラックパッドの慣性スクロールは高頻度の小さいdelta (pixel modeで
 *   1〜15px/event) のストリームとして到達する。イベント毎にnav(±1)すると
 *   1回のスワイプで数十枚飛ぶ。単純な閾値だけでも閾値を超えるたびに
 *   発火し続ける。そのため「1ジェスチャ=1送り」のlatchを置く。
 * - latchはジェスチャの無通信期間 (既定250ms) で解除する。離散的な
 *   マウスノッチ (≈100px/イベント) はノッチ間が静穏期間になるため、
 *   ゆっくり回すノッチ1回ごとに1枚送れる。
 * - 閾値は60: 標準的なマウスノッチ (WebKIT/Chrome系のpixel modeで
 *   100〜120px) の単発イベントで確実に発火しつつ、誤触れ程度の微小累積
 *   (数十px未満) は送りにしない。
 * - 修飾キー付きwheel (Alt/Ctrl/Meta=ズーム・pinch) はこの解釈の対象外
 *   (ImageViewer側で分岐)。modifierジェスチャの後もplain累積を引き継が
 *   ないよう、呼び出し側でstateをリセットする。
 */

export const WHEEL_NAV_THRESHOLD = 60;
export const WHEEL_NAV_GESTURE_TIMEOUT_MS = 250;

export type WheelNavState = {
  /** 現ジェスチャのdeltaY累積。 */
  accumulated: number;
  /** このジェスチャで既に送りを発火したか (発火後は静穏まで追加送りしない)。 */
  latched: boolean;
  /** 直前イベントの時刻 (ms)。無通信期間のジェスチャ終了検出用。 */
  lastEventMs: number;
};

export type WheelNavStep = -1 | 0 | 1;

export type WheelNavAdvance = {
  state: WheelNavState;
  /** このイベントで発生した送り (0=なし。負=前・正=次)。 */
  step: WheelNavStep;
};

export function initialWheelNavState(): WheelNavState {
  return { accumulated: 0, latched: false, lastEventMs: -1 };
}

/**
 * wheelイベント1個を解釈し、次状態と送りstepを返す (純粋 — stateは破壊しない)。
 * nowMsには単調時計 (performance.now等) を渡す。
 */
export function advanceWheelNav(
  prev: WheelNavState,
  deltaY: number,
  nowMs: number,
  options: { threshold?: number; gestureTimeoutMs?: number } = {},
): WheelNavAdvance {
  const threshold = options.threshold ?? WHEEL_NAV_THRESHOLD;
  const gestureTimeoutMs = options.gestureTimeoutMs ?? WHEEL_NAV_GESTURE_TIMEOUT_MS;

  if (typeof deltaY !== "number" || !Number.isFinite(deltaY) || deltaY === 0) {
    return { state: prev, step: 0 };
  }

  // 直前イベントから静穏期間を空けたら新ジェスチャ (累積もlatchもリセット)
  const isNewGesture =
    prev.lastEventMs < 0 || nowMs - prev.lastEventMs > gestureTimeoutMs;
  const state: WheelNavState = isNewGesture
    ? { accumulated: 0, latched: false, lastEventMs: nowMs }
    : { ...prev, lastEventMs: nowMs };

  if (state.latched) {
    // 同一ジェスチャ内の追いスクロール (方向反転を含む) は静穏まで飲み込む
    return { state, step: 0 };
  }

  state.accumulated += deltaY;
  if (Math.abs(state.accumulated) >= threshold) {
    const step: WheelNavStep = state.accumulated > 0 ? 1 : -1;
    state.accumulated = 0;
    state.latched = true;
    return { state, step };
  }
  return { state, step: 0 };
}
