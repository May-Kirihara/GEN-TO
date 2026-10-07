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

/** UIUX-1: ホイールスクロール速度係数の範囲と刻み。UIUX-3で既定を2.0へ
 * (1.0=ネイティブ。保存済み値は移行しない — no-migration)。 */
export const SCROLL_SPEED_MIN = 0.5;
export const SCROLL_SPEED_MAX = 5;

/** 左ペインのタブ (UIUX-1)。 */
export type LeftPaneTab = "folders" | "collections" | "library";

/** InfoPanelセクションの安定ID (UIUX-5 WP5 — 折りたたみ永続化のキー)。 */
export const INFO_PANEL_SECTION_IDS = [
  "section-file",
  "section-meta-photo",
  "section-meta-generation",
  "section-meta-raw",
  "section-triage",
  "section-memo",
  "section-session",
] as const;

export type LayoutPrefs = {
  leftVisible: boolean;
  rightVisible: boolean;
  leftWidth: number;
  rightWidth: number;
  thumbSize: ThumbCellClass;
  scrollSpeed: number;
  leftTab: LeftPaneTab;
  /** UIUX-3: 検索行の折りたたみ (true=展開)。 */
  searchExpanded: boolean;
  /** UIUX-5 WP5: InfoPanelの折りたたみ中セクションID列。 */
  infoPanelCollapsed: string[];
};

export const DEFAULT_LAYOUT_PREFS: LayoutPrefs = {
  leftVisible: true,
  rightVisible: true,
  leftWidth: 240,
  rightWidth: 280,
  thumbSize: "s256",
  scrollSpeed: 2,
  leftTab: "folders",
  searchExpanded: true,
  /** 既定は埋め込みメタ3種+コレクションメモを折りたたみ (ファイル情報・
   * 評価・選別セッションは展開 — セッション開始導線の到達性)。 */
  infoPanelCollapsed: [
    "section-meta-photo",
    "section-meta-generation",
    "section-meta-raw",
    "section-memo",
  ],
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

/** セクションID列の正規化 (UIUX-5 WP5)。配列以外は既定へ。未知IDは除外
 * (前方互換)・重複除去。順序は保存時のまま維持する。 */
function coerceSectionIds(value: unknown): string[] {
  if (!Array.isArray(value)) {
    return [...DEFAULT_LAYOUT_PREFS.infoPanelCollapsed];
  }
  const known = new Set<string>(INFO_PANEL_SECTION_IDS);
  const ids: string[] = [];
  for (const id of value) {
    if (typeof id === "string" && known.has(id) && !ids.includes(id)) {
      ids.push(id);
    }
  }
  return ids;
}

/** セクション折りたたみの排他トグル (純粋関数)。未知のIDはリスト不変。 */
export function toggleSection(list: string[], id: string): string[] {
  if (!(INFO_PANEL_SECTION_IDS as readonly string[]).includes(id)) {
    return list;
  }
  return list.includes(id) ? list.filter((section) => section !== id) : [...list, id];
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
      searchExpanded: coerceBool(value.searchExpanded, DEFAULT_LAYOUT_PREFS.searchExpanded),
      infoPanelCollapsed: coerceSectionIds(value.infoPanelCollapsed),
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
        searchExpanded: coerceBool(prefs.searchExpanded, DEFAULT_LAYOUT_PREFS.searchExpanded),
        infoPanelCollapsed: coerceSectionIds(prefs.infoPanelCollapsed),
      }),
    );
  }
}
