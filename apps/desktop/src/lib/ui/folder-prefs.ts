/**
 * 閲覧位置 (前回フォルダ・ツリー展開状態) の保存・復元 (MVP WP-4)。
 *
 * 再起動時の復元方針: 起動時の走査対象は `--dataset` (決定論的。
 * 計測・巨大フォルダの意図しない起動スキャンを避ける)。前回フォルダは
 * ツリー上の選択ハイライトとして復元し、ワンクリックで戻れるようにする。
 * スクロール位置はフォルダ別キーでScrollPositionStoreが復元する。
 */

export const FOLDER_STORAGE_KEY = "gento:folders:v1";

export type FolderPrefs = {
  /** 前回閲覧していたフォルダの絶対パス。 */
  lastFolder: string | null;
  /** ツリーで展開していたノードのパス (展開状態の復元用)。 */
  expanded: string[];
};

export const DEFAULT_FOLDER_PREFS: FolderPrefs = {
  lastFolder: null,
  expanded: [],
};

/** 展開ノード上限 (際限ない保存を防ぐ。超過分は古い順に切り捨て)。 */
export const MAX_EXPANDED_NODES = 500;

export type KeyValueStorage = {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
};

export function parseFolderPrefs(raw: string | null): FolderPrefs | null {
  if (raw === null) {
    return null;
  }
  try {
    const value = JSON.parse(raw) as Record<string, unknown>;
    if (typeof value !== "object" || value === null) {
      return null;
    }
    const lastFolder =
      typeof value.lastFolder === "string" && value.lastFolder.length > 0
        ? value.lastFolder
        : null;
    const expanded = Array.isArray(value.expanded)
      ? value.expanded.filter((item): item is string => typeof item === "string")
      : [];
    return { lastFolder, expanded: expanded.slice(-MAX_EXPANDED_NODES) };
  } catch {
    return null;
  }
}

export class FolderPrefsStore {
  constructor(private readonly storage: KeyValueStorage) {}

  load(): FolderPrefs {
    return parseFolderPrefs(this.storage.getItem(FOLDER_STORAGE_KEY)) ?? {
      ...DEFAULT_FOLDER_PREFS,
    };
  }

  save(prefs: FolderPrefs): void {
    this.storage.setItem(
      FOLDER_STORAGE_KEY,
      JSON.stringify({
        lastFolder: prefs.lastFolder,
        expanded: prefs.expanded.slice(-MAX_EXPANDED_NODES),
      }),
    );
  }
}
