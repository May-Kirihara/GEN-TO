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

// ===== UIUX-1: パンくずへのパス直接入力 =====

export type PastedPathResult = { ok: true; path: string } | { ok: false; error: string };

/** 絶対パス判定: POSIX (/) / Windowsドライブ (X:・X:\・X:/) / UNC (¥…)。
 * (単一英字+コロンはドライブ文字として扱い、URLスキームと区別する) */
function isAbsolutePathLike(path: string): boolean {
  if (path.startsWith("/") || path.startsWith("\\")) {
    return true;
  }
  return /^[A-Za-z]:(?:[\\/]|$)/.test(path);
}

/**
 * パンくず入力の正規化 (保守的 — R-E仕様)。
 *
 * - 前後空白のみtrim・**対になった**同種引用符のみ除去
 * - 末尾スラッシュ (バックスラッシュ含む) は正規化。ただしルート自体
 *   (`/`・`C:\`) は保存
 * - 改行・NULは拒否。`file://` URIは非対応として通知 (percent-decodeはしない)
 * - 相対パス・`~`は拒否 (展開しない)。`..`/`.` や symlink の解決は
 *   backend (open_folder) へ委譲し、UIでは字句判断しない
 * - 存在・種別・権限・大文字小差の正本は既存のopen-folder結果
 */
export function normalizePastedPath(raw: string): PastedPathResult {
  let value = raw.trim();
  if (
    value.length >= 2 &&
    ((value.startsWith('"') && value.endsWith('"')) ||
      (value.startsWith("'") && value.endsWith("'")))
  ) {
    value = value.slice(1, -1);
  }
  if (value === "") {
    return { ok: false, error: "パスを入力してください" };
  }
  if (/[\r\n]/.test(value) || value.includes("\x00")) {
    return { ok: false, error: "パスに改行やNUL文字を含めることはできません" };
  }
  if (/^file:\/\//i.test(value)) {
    return { ok: false, error: "file:// URIには対応していません。絶対パスを入力してください" };
  }
  if (/^[A-Za-z][A-Za-z0-9+.-]{1,}:/.test(value)) {
    // ドライブ文字 (C: / C:\) 以外のコロン形式 = URLスキーム等
    // (単一英字+コロンはドライブとして扱うため除外)
    return { ok: false, error: "URLとしては指定できません。絶対パスを入力してください" };
  }
  if (!isAbsolutePathLike(value)) {
    return { ok: false, error: "絶対パス (/ から始まるパス) を入力してください" };
  }
  // 末尾区切り正規化: ルート自体 (/・C:\) は保存、それ以外は削除
  const isPosixRoot = value === "/";
  const isWindowsDriveRoot = /^[A-Za-z]:[\\/]?$/.test(value);
  if (!isPosixRoot && !isWindowsDriveRoot) {
    value = value.replace(/[\\/]+$/, "");
  }
  return { ok: true, path: value };
}
