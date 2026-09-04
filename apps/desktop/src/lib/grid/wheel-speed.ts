/**
 * ホイールスクロール速度 (UIUX-1 / R-C)。
 *
 * - 係数は layout-prefs (localStorage) に保存。既定1.0 = ネイティブ挙動で
 *   **リスナ自体を登録しない** (係数≠1.0時のみactionが登録する)
 * - deltaMode別の正規化: pixel=そのまま / line=行高さ換算 / page=viewport高さ換算
 * - ctrlKey (ピンチズーム) は素通り。deltaXは無視 (縦スクロールコンテナ)
 * - `..`等の解決は無関係。端でのチェイニング阻害はしない (scrollByが自然に停止)
*/
import { coerceScrollSpeed, SCROLL_SPEED_MAX, SCROLL_SPEED_MIN } from "../ui/layout-prefs";

export { coerceScrollSpeed, SCROLL_SPEED_MIN, SCROLL_SPEED_MAX };

export type WheelMetrics = {
  /** 1行の高さ (px)。lineモード換算に使う。 */
  lineHeight: number;
  /** viewport高さ (px)。pageモード換算に使う。 */
  pageHeight: number;
};

/**
 * スクロール量 (px) を計算。処理しない場合はnull
 * (係数1.0・ctrlKey付き・未知の入力)。
 */
export function computeWheelScroll(
  deltaY: number,
  deltaMode: number,
  factor: number,
  metrics: WheelMetrics,
  opts: { ctrlKey?: boolean } = {},
): number | null {
  if (opts.ctrlKey === true || factor === 1 || !Number.isFinite(deltaY) || deltaY === 0) {
    return null;
  }
  let unit: number;
  if (deltaMode === 1) {
    unit = Math.max(1, metrics.lineHeight);
  } else if (deltaMode === 2) {
    unit = Math.max(1, metrics.pageHeight);
  } else {
    // 0 (pixel) と未知の値はpixel扱い (前方互換)
    unit = 1;
  }
  return deltaY * unit * factor;
}

/**
 * 係数≠1.0のときだけ `{passive:false}` でwheelリスナを登録するSvelte action。
 *
 * - 同一listener identityで除去・再登録するため、update時の二重登録は起きない
 * - Svelteテンプレートのonwheel指定はpassiveが強制されるため、
 *   preventDefaultが必要な本機能はaction経由で登録する (R-C)
 */
export type ConditionalWheelParams = {
  enabled: boolean;
  handler: (event: WheelEvent) => void;
};

export function conditionalWheel(
  node: HTMLElement,
  params: ConditionalWheelParams,
): {
  update(next: ConditionalWheelParams): void;
  destroy(): void;
} {
  let current = params;
  const listener = (event: WheelEvent): void => {
    current.handler(event);
  };
  function sync(): void {
    node.removeEventListener("wheel", listener);
    if (current.enabled) {
      node.addEventListener("wheel", listener, { passive: false });
    }
  }
  sync();
  return {
    update(next: ConditionalWheelParams): void {
      current = next;
      sync();
    },
    destroy(): void {
      node.removeEventListener("wheel", listener);
    },
  };
}
