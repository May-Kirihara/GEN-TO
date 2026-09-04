/**
 * 3ペインレイアウト設定の保存・復元 (MVP WP-4)。
 *
 * - ペイン表示/非表示・幅・サムネイルサイズクラスをlocalStorageへ永続化
 *   (Key-Value Storage注入でテスト可能。製品設定体系はWP-15)。
 * - 破損・欠損時は安全な既定値へフォールバックする。
 * - 幅はクランプ範囲外の値も受け取らない (復元時に改めてクランプ)。
 */

import type { ThumbCellClass } from "../grid/layout";

export const LAYOUT_STORAGE_KEY = "gento:layout:v1";

/** サイドペイン幅のクランプ範囲 (px)。 */
export const PANE_MIN_WIDTH = 160;
export const PANE_MAX_WIDTH = 520;

/** UIUX-1: ホイールスクロール速度係数の範囲と刻み。既定1.0=ネイティブ。 */
export const SCROLL_SPEED_MIN = 0.5;
export const SCROLL_SPEED_MAX = 5;

/** 左ペインのタブ (UIUX-1)。 */
export type LeftPaneTab = "folders" | "collections" | "library";

export type LayoutPrefs = {
  leftVisible: boolean;
  rightVisible: boolean;
  leftWidth: number;
  rightWidth: number;
  thumbSize: ThumbCellClass;
  scrollSpeed: number;
  leftTab: LeftPaneTab;
};

export const DEFAULT_LAYOUT_PREFS: LayoutPrefs = {
  leftVisible: true,
  rightVisible: true,
  leftWidth: 240,
  rightWidth: 280,
  thumbSize: "s256",
  scrollSpeed: 1,
  leftTab: "folders",
};

export type KeyValueStorage = {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
};

export function clampPaneWidth(width: number): number {
  if (!Number.isFinite(width)) {
    return DEFAULT_LAYOUT_PREFS.leftWidth;
  }
  return Math.min(PANE_MAX_WIDTH, Math.max(PANE_MIN_WIDTH, Math.round(width)));
}

/** 未知の値は既定へ落とす (前方互換: 不明なサイズクラスは無視)。 */
function coerceThumbSize(value: unknown): ThumbCellClass {
  return value === "s512" ? "s512" : "s256";
}

function coerceBool(value: unknown, fallback: boolean): boolean {
  return typeof value === "boolean" ? value : fallback;
}

function coerceWidth(value: unknown, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value)
    ? clampPaneWidth(value)
    : fallback;
}

/** 係数は0.5–5.0へクランプし0.1刻みへ丸める (スライダ入力の桁落ち吸収)。 */
export function coerceScrollSpeed(value: unknown): number {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return DEFAULT_LAYOUT_PREFS.scrollSpeed;
  }
  const clamped = Math.min(SCROLL_SPEED_MAX, Math.max(SCROLL_SPEED_MIN, value));
  return Math.round(clamped * 10) / 10;
}

/** 未知のタブidは既定へ (前方互換)。 */
function coerceLeftTab(value: unknown): LeftPaneTab {
  return value === "collections" || value === "library" ? value : "folders";
}

/** 保存値のパース。不正・破損時はnull (呼び出し側は既定値を使う)。 */
export function parseLayoutPrefs(raw: string | null): LayoutPrefs | null {
  if (raw === null) {
    return null;
  }
  try {
    const value = JSON.parse(raw) as Record<string, unknown>;
    if (typeof value !== "object" || value === null) {
      return null;
    }
    return {
      leftVisible: coerceBool(value.leftVisible, DEFAULT_LAYOUT_PREFS.leftVisible),
      rightVisible: coerceBool(value.rightVisible, DEFAULT_LAYOUT_PREFS.rightVisible),
      leftWidth: coerceWidth(value.leftWidth, DEFAULT_LAYOUT_PREFS.leftWidth),
      rightWidth: coerceWidth(value.rightWidth, DEFAULT_LAYOUT_PREFS.rightWidth),
      thumbSize: coerceThumbSize(value.thumbSize),
      scrollSpeed: coerceScrollSpeed(value.scrollSpeed),
      leftTab: coerceLeftTab(value.leftTab),
    };
  } catch {
    return null;
  }
}

export class LayoutPrefsStore {
  constructor(private readonly storage: KeyValueStorage) {}

  load(): LayoutPrefs {
    return parseLayoutPrefs(this.storage.getItem(LAYOUT_STORAGE_KEY)) ?? {
      ...DEFAULT_LAYOUT_PREFS,
    };
  }

  save(prefs: LayoutPrefs): void {
    this.storage.setItem(
      LAYOUT_STORAGE_KEY,
      JSON.stringify({
        leftVisible: prefs.leftVisible,
        rightVisible: prefs.rightVisible,
        leftWidth: clampPaneWidth(prefs.leftWidth),
        rightWidth: clampPaneWidth(prefs.rightWidth),
        thumbSize: prefs.thumbSize,
        scrollSpeed: coerceScrollSpeed(prefs.scrollSpeed),
        leftTab: coerceLeftTab(prefs.leftTab),
      }),
    );
  }
}
