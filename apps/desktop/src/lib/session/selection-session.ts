/**
 * 選別セッション (MVP WP-4 / 仕様§9.3の足場)。
 *
 * - 順序付きasset ID列・現在位置・確認済み位置集合を保持し、
 *   確認済み/採用/除外/保留の件数を表示できる (採用・除外マーク自体は
 *   WP-5。ここではカタログ項目のtriage値を集計するのみ)。
 * - 途中終了→再開: localStorageへ永続化 (uuidをセッション名とする)。
 * - 位置はグリッドの選択フォーカスと同期する設計: Appが選択変化を
 *   noteFocus(id) へ流し、セッション内IDなら位置と確認済みを更新する。
 */

export const SESSION_STORAGE_KEY = "gento:session:v1";

export type SessionTriage = "keep" | "reject" | "undecided";

/** カタログ項目のtriage表現 (gento-core Triageの文字列化と同じ)。 */
export type TriageLike = "keep" | "reject" | "undecided";

/** 表示ソート値 (backend CatalogSort のIPC文字列と同じ)。 */
export type CatalogSortValue = "name" | "date_desc" | "size_desc";

export const CATALOG_SORT_VALUES: readonly CatalogSortValue[] = [
  "name",
  "date_desc",
  "size_desc",
];

/**
 * セッションの開始 origin 詳細 (QA-WP4 Major-2)。
 * adhoc (一時走査) のIDはフォルダ・ソート・走査順の組でしか安定しない
 * ため、再開時の復元・検証に必要な情報を保存する。
 * library は安定AssetIdなので復元不要。
 */
export type SessionOriginDetail =
  | {
      kind: "adhoc-folder";
      folderPath: string;
      sort: CatalogSortValue;
    }
  | {
      kind: "adhoc-selection";
      folderPath: string;
      sort: CatalogSortValue;
    }
  | { kind: "library" }
  | {
      /** コレクション表示から開始 (WP-6。spec §9.3)。再開時は同一
       * コレクションを再表示してからID解決する。 */
      kind: "collection";
      collectionId: string;
    };

export type PersistedSession = {
  v: 1;
  /** セッション名 (uuid)。 */
  id: string;
  ids: string[];
  position: number;
  /** 確認済みの位置index (昇順を保証しない。Set的に扱う)。 */
  visited: number[];
  origin: string;
  /** 開始時のorigin詳細 (保存時の構造。旧データではnull)。 */
  detail: SessionOriginDetail | null;
  updatedAt: number;
};

export type SessionCounts = {
  total: number;
  /** 現在位置 (1-based表示はUI側)。 */
  position: number;
  confirmed: number;
  keep: number;
  reject: number;
  undecided: number;
};

export type KeyValueStorage = {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
};

export function generateSessionId(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return crypto.randomUUID();
  }
  // テスト・非セキュア文脈フォールバック (一意性のみで十分)
  return `session-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}

function parseOriginDetail(
  value: unknown,
): SessionOriginDetail | null {
  if (typeof value !== "object" || value === null) {
    return null;
  }
  const kind = (value as Record<string, unknown>)["kind"];
  if (kind === "library") {
    return { kind: "library" };
  }
  if (kind === "collection") {
    const collectionId = (value as Record<string, unknown>)["collectionId"];
    if (typeof collectionId !== "string" || collectionId.length === 0) {
      return null;
    }
    return { kind: "collection", collectionId };
  }
  if (kind !== "adhoc-folder" && kind !== "adhoc-selection") {
    return null;
  }
  const folderPath = (value as Record<string, unknown>)["folderPath"];
  const sort = (value as Record<string, unknown>)["sort"];
  if (typeof folderPath !== "string" || folderPath.length === 0) {
    return null;
  }
  if (
    typeof sort !== "string" ||
    !CATALOG_SORT_VALUES.includes(sort as CatalogSortValue)
  ) {
    return null;
  }
  return { kind, folderPath, sort: sort as CatalogSortValue };
}

/** 保存値のパース。不正・非対応バージョンはnull。 */
export function parsePersistedSession(raw: string | null): PersistedSession | null {
  if (raw === null) {
    return null;
  }
  try {
    const value = JSON.parse(raw) as Record<string, unknown>;
    if (
      typeof value !== "object" ||
      value === null ||
      value.v !== 1 ||
      typeof value.id !== "string" ||
      !Array.isArray(value.ids) ||
      !value.ids.every((id) => typeof id === "string") ||
      typeof value.position !== "number" ||
      !Array.isArray(value.visited) ||
      !value.visited.every((p) => typeof p === "number" && Number.isInteger(p) && p >= 0) ||
      typeof value.origin !== "string"
    ) {
      return null;
    }
    const ids = value.ids as string[];
    const position = Math.min(Math.max(0, Math.trunc(value.position)), Math.max(0, ids.length - 1));
    const visited = (value.visited as number[]).filter((p) => p < ids.length);
    return {
      v: 1,
      id: value.id,
      ids,
      position,
      visited,
      origin: value.origin,
      detail: parseOriginDetail(value.detail),
      updatedAt: typeof value.updatedAt === "number" ? value.updatedAt : 0,
    };
  } catch {
    return null;
  }
}

/** adhocセッション再開前の比較対象 (現在のUI状態)。 */
export type ResumeCurrentState = {
  source: "temp" | "library";
  folderPath: string | null;
  sort: CatalogSortValue;
};

/** 再開の判定結果。 */
export type ResumePlan =
  | { action: "resume" }
  /** 保存時のフォルダ・ソートへ戻してから再開する。 */
  | { action: "restore"; folderPath: string; sort: CatalogSortValue };

/**
 * adhocセッションを再開してよいか判定する (QA-WP4 Major-2)。
 * source・フォルダ・ソートのいずれかが保存時と異なる場合は、保存済みの
 * フルダ+ソートを復元してからID解決する (誤画像へのサイレント再開防止)。
 * library由来・旧データ (detailなし) はそのまま再開してよい。
 */
export function planAdhocResume(
  persisted: PersistedSession,
  current: ResumeCurrentState,
): ResumePlan {
  const detail = persisted.detail;
  if (detail === null || detail.kind === "library" || detail.kind === "collection") {
    // collection由来のIDはstable AssetIdのためフォルダ復元は不要
    // (コレクション再表示はAppのresumeSessionが行う)
    return { action: "resume" };
  }
  const sameFolder =
    current.source === "temp" && current.folderPath === detail.folderPath;
  if (sameFolder && current.sort === detail.sort) {
    return { action: "resume" };
  }
  return { action: "restore", folderPath: detail.folderPath, sort: detail.sort };
}

/**
 * 保存時の件数と現在件数のずれが許容範囲 (既定5%)を超えたか。
 * adhocフォルダ全体セッションの再開中止判定に使う。
 */
export function sessionCountDrifted(
  savedCount: number,
  currentTotal: number,
  threshold = 0.05,
): boolean {
  if (savedCount <= 0) {
    return false;
  }
  return Math.abs(currentTotal - savedCount) / savedCount > threshold;
}

function toMessage(value: unknown): string {
  return value instanceof Error ? value.message : String(value);
}

export class SelectionSessionStore {
  session: PersistedSession | null = null;
  lastError: string | null = null;
  version = 0;

  private readonly changeListeners = new Set<() => void>();
  private saveTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(
    private readonly storage: KeyValueStorage,
    private readonly opts?: { saveDebounceMs?: number },
  ) {}

  onChange(listener: () => void): () => void {
    this.changeListeners.add(listener);
    return () => {
      this.changeListeners.delete(listener);
    };
  }

  private notify(): void {
    this.version++;
    for (const listener of this.changeListeners) {
      listener();
    };
  }

  get active(): boolean {
    return this.session !== null;
  }

  /** 非アクティブ時の再開候補 (「再開 (N件)」表示用)。 */
  peekPersisted(): { origin: string; count: number; updatedAt: number } | null {
    const persisted = parsePersistedSession(this.storage.getItem(SESSION_STORAGE_KEY));
    if (persisted === null || persisted.ids.length === 0) {
      return null;
    }
    return {
      origin: persisted.origin,
      count: persisted.ids.length,
      updatedAt: persisted.updatedAt,
    };
  }

  /** 永続化されたセッションの全体 (再開計画・検証用)。 */
  peekPersistedSession(): PersistedSession | null {
    return parsePersistedSession(this.storage.getItem(SESSION_STORAGE_KEY));
  }

  /** セッション開始。idsは表示順 (順序付きasset列)。 */
  start(
    ids: string[],
    origin: string,
    position = 0,
    detail: SessionOriginDetail | null = null,
  ): void {
    if (ids.length === 0) {
      this.lastError = "セッション対象が空です";
      this.notify();
      return;
    }
    this.lastError = null;
    const clamped = Math.min(Math.max(0, position), ids.length - 1);
    this.session = {
      v: 1,
      id: generateSessionId(),
      ids: [...ids],
      position: clamped,
      visited: [clamped],
      origin,
      detail,
      updatedAt: Date.now(),
    };
    this.scheduleSave();
    this.notify();
  }

  /** 永続化されたセッションを再開。対象が無ければfalse。 */
  resume(): boolean {
    const persisted = parsePersistedSession(this.storage.getItem(SESSION_STORAGE_KEY));
    if (persisted === null || persisted.ids.length === 0) {
      return false;
    }
    this.session = persisted;
    this.notify();
    return true;
  }

  /** 選択フォーカスのidを反映 (セッション内なら位置を更新)。 */
  noteFocus(id: string): void {
    if (this.session === null) {
      return;
    }
    const index = this.session.ids.indexOf(id);
    if (index < 0 || index === this.session.position) {
      return;
    }
    this.moveTo(index, { silent: true });
  }

  /** 位置移動。確認済みに含める。 */
  moveTo(position: number, opts?: { silent?: boolean }): void {
    if (this.session === null) {
      return;
    }
    const clamped = Math.min(Math.max(0, position), this.session.ids.length - 1);
    if (!this.session.visited.includes(clamped)) {
      this.session.visited = [...this.session.visited, clamped];
    }
    this.session.position = clamped;
    this.session.updatedAt = Date.now();
    this.scheduleSave();
    if (opts?.silent !== true) {
      this.notify();
    } else {
      // silentでも保存対象は変化するため版だけ進める (UI再描画用)
      this.version++;
    }
  }

  next(): void {
    if (this.session !== null) {
      this.moveTo(this.session.position + 1);
    }
  }

  prev(): void {
    if (this.session !== null) {
      this.moveTo(this.session.position - 1);
    }
  }

  /** 現在位置のid。 */
  currentId(): string | null {
    return this.session?.ids[this.session.position] ?? null;
  }

  /** id → セッション内位置 (グリッドインデックス→セッション位置の変換用)。 */
  positionOf(id: string): number | null {
    if (this.session === null) {
      return null;
    }
    const index = this.session.ids.indexOf(id);
    return index < 0 ? null : index;
  }

  /** 件数集計。triageOfはid→triage (項目未解決はundecided扱い)。 */
  counts(triageOf: (id: string) => TriageLike | undefined): SessionCounts | null {
    const session = this.session;
    if (session === null) {
      return null;
    }
    let keep = 0;
    let reject = 0;
    let undecided = 0;
    for (const id of session.ids) {
      const triage = triageOf(id) ?? "undecided";
      if (triage === "keep") {
        keep++;
      } else if (triage === "reject") {
        reject++;
      } else {
        undecided++;
      }
    }
    return {
      total: session.ids.length,
      position: session.position,
      confirmed: session.visited.length,
      keep,
      reject,
      undecided,
    };
  }

  /** セッション終了 (永続化も削除)。 */
  end(): void {
    this.session = null;
    this.storage.removeItem(SESSION_STORAGE_KEY);
    if (this.saveTimer !== null) {
      clearTimeout(this.saveTimer);
      this.saveTimer = null;
    }
    this.notify();
  }

  /**
   * 再開検証失敗時の非活性化 (QA-WP4)。永続化されたデータは残す
   * (誤再開防止だけで、保存済みセッション自体は消さない)。
   */
  abort(): void {
    this.session = null;
    if (this.saveTimer !== null) {
      clearTimeout(this.saveTimer);
      this.saveTimer = null;
    }
    this.notify();
  }

  /** 保留中の書き込みを確定 (破棄時)。 */
  flush(): void {
    if (this.saveTimer !== null) {
      clearTimeout(this.saveTimer);
      this.saveTimer = null;
    }
    if (this.session !== null) {
      this.persist();
    }
  }

  private scheduleSave(): void {
    if (this.saveTimer !== null) {
      return;
    }
    this.saveTimer = setTimeout(() => {
      this.saveTimer = null;
      if (this.session !== null) {
        this.persist();
      }
    }, this.opts?.saveDebounceMs ?? 400);
  }

  private persist(): void {
    if (this.session === null) {
      return;
    }
    try {
      this.storage.setItem(SESSION_STORAGE_KEY, JSON.stringify(this.session));
    } catch (error) {
      this.lastError = toMessage(error);
    }
  }
}
