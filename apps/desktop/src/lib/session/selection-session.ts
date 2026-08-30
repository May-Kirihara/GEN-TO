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

export type PersistedSession = {
  v: 1;
  /** セッション名 (uuid)。 */
  id: string;
  ids: string[];
  position: number;
  /** 確認済みの位置index (昇順を保証しない。Set的に扱う)。 */
  visited: number[];
  origin: string;
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
      updatedAt: typeof value.updatedAt === "number" ? value.updatedAt : 0,
    };
  } catch {
    return null;
  }
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

  /** セッション開始。idsは表示順 (順序付きasset列)。 */
  start(ids: string[], origin: string, position = 0): void {
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
