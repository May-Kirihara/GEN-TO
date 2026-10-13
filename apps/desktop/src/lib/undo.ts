/**
 * メタデータ操作のUndoスタック (MVP WP-5)。
 *
 * - 直近N件 (既定50) の操作の逆適用関数を保持し、Ctrl+Zで巻き戻す。
 * - スコープは**メタデータ操作のみ** (評価・選別・お気に入り・タグ・
 *   キャプション)。ファイル操作 (コピー・移動・ゴミ箱) のUndoは
 *   WP-7でfile_operations記録とともに統合される (計画§14)。
 * - 逆適用関数は操作実行時にbefore値を捕捉したクロージャ —
 *   「巻き戻し = before値への再set」で冪等。
 * - Undo実行で新しい操作は行われない (redoなし。MVPスコープ)。
 *   Undo後に通常操作が行われたらそれもスタックへ積まれる
 *   (線形履歴 — この操作の巻き戻しも可能)。
 */

/** Undo可能な操作1件。labelは通知表示用。 */
export type UndoEntry = {
  label: string;
  /** 逆適用 (before値へ戻す)。成功時に通知を上書きする文
   *  (file-ops undoの手動復元案内など) を返せる — 呼び出しの結果に
   *  紐付けるため、非同期並走でも取り違えない。失敗は呼び出し側で通知する。 */
  undo: () => Promise<string | null | void>;
};

/** undo() 成功の応答 (通知上書き文。上書きが無ければnull)。 */
export type UndoSucceeded = {
  done: true;
  notice: string | null;
};

export const DEFAULT_UNDO_LIMIT = 50;

export class UndoStack {
  private readonly entries: UndoEntry[] = [];
  private readonly limit: number;
  private readonly changeListeners = new Set<() => void>();
  version = 0;
  /** 直近のundo失敗 (通知表示用)。 */
  lastError: string | null = null;

  constructor(limit: number = DEFAULT_UNDO_LIMIT) {
    this.limit = Math.max(1, limit);
  }

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
    }
  }

  get depth(): number {
    return this.entries.length;
  }

  canUndo(): boolean {
    return this.entries.length > 0;
  }

  /** 次にundoされる操作のラベル (UI表示用)。 */
  peekLabel(): string | null {
    return this.entries.length > 0 ? this.entries[this.entries.length - 1].label : null;
  }

  /** 操作を登録 (実行自体は呼び出し側が行う — ここには逆適用のみ預ける)。 */
  push(entry: UndoEntry): void {
    this.entries.push(entry);
    if (this.entries.length > this.limit) {
      this.entries.splice(0, this.entries.length - this.limit);
    }
    this.notify();
  }

  /** 直近の操作を巻き戻す。スタック空・逆適用失敗はfalse。 */
  async undo(): Promise<UndoSucceeded | false> {
    const entry = this.entries.pop();
    if (entry === undefined) {
      return false;
    }
    try {
      const notice = await entry.undo();
      this.lastError = null;
      this.notify();
      return { done: true, notice: notice ?? null };
    } catch (error) {
      // 失敗したエントリは戻さない (再試行で無限ループしないように)
      this.lastError = error instanceof Error ? error.message : String(error);
      this.notify();
      return false;
    }
  }

  /** 全履歴を破棄 (表示ソース切替等で文脈が変わった時)。 */
  clear(): void {
    if (this.entries.length > 0) {
      this.entries.length = 0;
      this.notify();
    }
  }
}
