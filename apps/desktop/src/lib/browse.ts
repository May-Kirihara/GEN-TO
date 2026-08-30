/**
 * 閲覧位置UIの純粋計算 (MVP WP-4)。
 *
 * - パンくず (パス分割・クリック可能部分)
 * - グリッド空状態の分岐 (空・読み取り不能・不在・スキャン中)
 */

export type Crumb = { name: string; path: string };

/** 絶対パスをパンくず部分へ分割 ("/" 自体は1要素)。 */
export function breadcrumbOf(path: string): Crumb[] {
  if (path === "") {
    return [];
  }
  if (path === "/") {
    return [{ name: "/", path: "/" }];
  }
  const parts = path.split("/").filter((part) => part.length > 0);
  const crumbs: Crumb[] = [{ name: "/", path: "/" }];
  let acc = "";
  for (const part of parts) {
    acc += `/${part}`;
    crumbs.push({ name: part, path: acc });
  }
  return crumbs;
}

export type EmptyStateKind = "scanning" | "folder-error" | "empty";

export type EmptyState = {
  kind: EmptyStateKind;
  /** 表示文言。 */
  message: string;
  /** folder-error時の生エラー (デバッグ表示用)。 */
  detail: string | null;
};

/**
 * グリッドが項目を表示できない理由を導出する。
 * - folderStatus: 現在フォルダの状態 (open_folder失敗時の文言)
 * - 「画像が見つからない」は未対応形式のみのフォルダと共通表示とする
 */
export function deriveEmptyState(input: {
  scanning: boolean;
  total: number;
  folderError: string | null;
}): EmptyState | null {
  if (input.total > 0) {
    return null;
  }
  if (input.folderError !== null) {
    return {
      kind: "folder-error",
      message: "このフォルダを表示できません",
      detail: input.folderError,
    };
  }
  if (input.scanning) {
    return { kind: "scanning", message: "読み込み中…", detail: null };
  }
  return {
    kind: "empty",
    message: "画像が見つかりません (対応形式の画像がない可能性があります)",
    detail: null,
  };
}

/** mtime (エポックms) の表示用フォーマット (ロケール依存)。 */
export function formatTimestamp(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) {
    return "—";
  }
  return new Date(ms).toLocaleString();
}
