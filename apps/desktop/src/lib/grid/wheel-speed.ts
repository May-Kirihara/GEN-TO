/**
 * ホイールスクロール速度 (UIUX-1 / R-C)。
 *
 * - 係数は layout-prefs (localStorage) に保存。既定1.0 = ネイティブ挙動で
 *   **リスナ自体を登録しない** (係数≠1.0時のみactionが登録する)
 * - deltaMode別の正規化: pixel=そのまま / line=行高さ換算 / page=viewport高さ換算
 *   (line高はCSS慣行値 — computed line-height。サムネ行extent非依存)
 * - ctrlKey (ピンチズーム) は素通り。deltaXは無視 (縦スクロールコンテナ)
 * - `..`等の解決は無関係。端の外向きdeltaではpreventDefaultしない
 *   (canScrollWithin判定 → 親スクロールへのチェイニングを阻害しない)
 */
import { coerceScrollSpeed, SCROLL_SPEED_MAX, SCROLL_SPEED_MIN } from "../ui/layout-prefs";

export { coerceScrollSpeed, SCROLL_SPEED_MIN, SCROLL_SPEED_MAX };

/** lineモード換算の行高フォールバック (px)。DOM慣行値 (normalize-wheel等と
 * 同一 — computed line-heightが使えない環境での1行相当)。 */
export const FALLBACK_LINE_HEIGHT = 16;

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
 * 現在位置からdelta方向へさらにスクロール可能か。
 * false (= 端の外向き、またはスクロール不能コンテナ) では
 * preventDefaultしないこと — 親スクロールへチェイニングさせる。
 */
export function canScrollWithin(
  delta: number,
  bounds: { scrollTop: number; clientHeight: number; scrollHeight: number },
): boolean {
  const max = bounds.scrollHeight - bounds.clientHeight;
  if (max <= 0 || !Number.isFinite(delta) || delta === 0) {
    return false;
  }
  return delta < 0 ? bounds.scrollTop > 0 : bounds.scrollTop < max;
}

/**
 * lineモード (DOM_DELTA_LINE) 換算の行高 (px) を解決する。
 * CSS慣行: `getComputedStyle(el).lineHeight` のpx値。非px ("normal"等)・
 * 不正値・取得不能ならFALLBACK_LINE_HEIGHT (16px/行)。
 * サムネイル行の実寸 (extent) には依存しない — デバイス・イベントモード
 * 非依存の正規化 (UIUX-1 r1 B2)。
 */
export function resolveLineHeight(computed: string | null | undefined): number {
  if (typeof computed !== "string") {
    return FALLBACK_LINE_HEIGHT;
  }
  const match = /^(-?[0-9]+(?:\.[0-9]+)?)px$/.exec(computed.trim());
  if (match === null) {
    return FALLBACK_LINE_HEIGHT;
  }
  const value = Number(match[1]);
  return Number.isFinite(value) && value > 0 ? value : FALLBACK_LINE_HEIGHT;
}

/**
 * 係数≠1.0のとき速度を適用するwheelハンドラの本体
 * (ThumbnailGrid.handleWheelから委譲。イベント経由のctrlKey受け渡しまで
 * 含めてテスト可能にするため分離 — r2)。
 */
export function applyWheelScroll(event: WheelEvent, factor: number): void {
  const element = event.currentTarget instanceof HTMLElement ? event.currentTarget : null;
  if (element === null) {
    return;
  }
  // line高はCSS慣行値 (computed line-height、非pxなら16px/行)。
  // サムネ行extent (~200px) には依存させない (r1 B2)。pixel/pageでは未使用。
  const lineHeight =
    event.deltaMode === 1 ? resolveLineHeight(getComputedStyle(element).lineHeight) : 1;
  const delta = computeWheelScroll(
    event.deltaY,
    event.deltaMode,
    factor,
    {
      lineHeight,
      pageHeight: element.clientHeight,
    },
    { ctrlKey: event.ctrlKey },
  );
  if (delta === null) {
    return;
  }
  if (!canScrollWithin(delta, element)) {
    return;
  }
  event.preventDefault();
  element.scrollBy({ top: delta });
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
  let attached = false;
  const listener = (event: WheelEvent): void => {
    current.handler(event);
  };
  function sync(): void {
    if (attached) {
      node.removeEventListener("wheel", listener);
      attached = false;
    }
    if (current.enabled) {
      node.addEventListener("wheel", listener, { passive: false });
      attached = true;
    }
  }
  sync();
  return {
    update(next: ConditionalWheelParams): void {
      current = next;
      sync();
    },
    destroy(): void {
      // 除去のみ。sync()だとenabled=trueのまま除去→即再登録する (r2)。
      if (attached) {
        node.removeEventListener("wheel", listener);
        attached = false;
      }
    },
  };
}
