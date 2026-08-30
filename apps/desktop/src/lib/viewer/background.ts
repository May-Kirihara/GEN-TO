/**
 * ビューアの透過背景切替 (MVP WP-3)。
 *
 * - 市松 (checker) / 白 / 黒 の3種。既定は市松。
 * - 設定はlocalStorageへ永続化 (製品設定体系はWP-15。それまでの
 *   ローカル保持)。localStorage不在環境 (テスト・SSR) は既定値で動作。
 */

export type ViewerBackground = "checker" | "white" | "black";

export const VIEWER_BACKGROUNDS: readonly ViewerBackground[] = [
  "checker",
  "white",
  "black",
];

const STORAGE_KEY = "gento.viewer.background";
export const DEFAULT_BACKGROUND: ViewerBackground = "checker";

/** localStorageへの安全アクセス (不在環境はnull)。 */
export function safeLocalStorage(): Storage | null {
  try {
    if (typeof window === "undefined" || typeof window.localStorage === "undefined") {
      return null;
    }
    return window.localStorage;
  } catch {
    // プライバシーモード等でアクセスが例外になる場合がある
    return null;
  }
}

/** 未知の値はnull (既定に落とす判定用)。 */
export function normalizeBackground(value: string | null): ViewerBackground | null {
  return VIEWER_BACKGROUNDS.find((kind) => kind === value) ?? null;
}

/** 保存値の読み込み (不正値・不在は既定の市松)。 */
export function loadBackground(storage: Storage | null): ViewerBackground {
  if (storage === null) {
    return DEFAULT_BACKGROUND;
  }
  try {
    return normalizeBackground(storage.getItem(STORAGE_KEY)) ?? DEFAULT_BACKGROUND;
  } catch {
    return DEFAULT_BACKGROUND;
  }
}

/** 保存 (失敗は無視 — 設定保持はベストエフォート)。 */
export function saveBackground(storage: Storage | null, background: ViewerBackground): void {
  if (storage === null) {
    return;
  }
  try {
    storage.setItem(STORAGE_KEY, background);
  } catch {
    // quota・シークレットモード等: 無視
  }
}

/** 次の背景へ循環する。 */
export function nextBackground(current: ViewerBackground): ViewerBackground {
  const index = VIEWER_BACKGROUNDS.indexOf(current);
  return VIEWER_BACKGROUNDS[(index + 1) % VIEWER_BACKGROUNDS.length] ?? DEFAULT_BACKGROUND;
}

/** 背景種別の表示名。 */
export function backgroundLabel(background: ViewerBackground): string {
  switch (background) {
    case "checker":
      return "市松";
    case "white":
      return "白";
    case "black":
      return "黒";
  }
}

/** stage要素に当てるCSS background値。 */
export function backgroundCss(background: ViewerBackground): string {
  switch (background) {
    case "checker":
      return "repeating-conic-gradient(light-dark(#e3e6ea, #171a21) 0% 25%, light-dark(#eceff3, #1d212b) 0% 50%) 0 / 24px 24px";
    case "white":
      return "#ffffff";
    case "black":
      return "#000000";
  }
}
