/**
 * スクロール位置の保存・復元 (WP-5 / 本体計画 §10-9)。
 *
 * - in-memory値は即時反映 (モード切替の復元用)
 * - localStorageへの書き込みはtrailing-edgeスロットル (既定250ms)
 * - モード別キーで独立管理
 */

export type KeyValueStorage = {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
};

type PendingEntry = { value: number; timer: ReturnType<typeof setTimeout> | null };

export class ScrollPositionStore {
  private readonly memory = new Map<string, number>();
  private readonly pending = new Map<string, PendingEntry>();

  constructor(
    private readonly storage: KeyValueStorage,
    private readonly opts?: { throttleMs?: number },
  ) {}

  private get throttleMs(): number {
    return this.opts?.throttleMs ?? 250;
  }

  /** スロットル付き保存。in-memoryは即時、storageは窓の末尾に1回。 */
  saveThrottled(key: string, value: number): void {
    this.memory.set(key, value);
    let entry = this.pending.get(key);
    if (!entry) {
      entry = { value, timer: null };
      this.pending.set(key, entry);
    }
    entry.value = value;
    if (entry.timer === null) {
      entry.timer = setTimeout(() => {
        entry.timer = null;
        this.storage.setItem(key, String(entry.value));
      }, this.throttleMs);
    }
  }

  /** 保留中の書き込みを即時確定 (破棄時など)。 */
  flush(): void {
    for (const [key, entry] of this.pending) {
      if (entry.timer !== null) {
        clearTimeout(entry.timer);
        entry.timer = null;
      }
      this.storage.setItem(key, String(entry.value));
    }
  }

  /** in-memory値のみ (モード切替の自動復元用)。 */
  peek(key: string): number | null {
    return this.memory.get(key) ?? null;
  }

  /** in-memory優先、なければlocalStorage (再起動後の明示復元用)。 */
  load(key: string): number | null {
    const inMemory = this.memory.get(key);
    if (inMemory !== undefined) {
      return inMemory;
    }
    const raw = this.storage.getItem(key);
    if (raw === null) {
      return null;
    }
    const parsed = Number.parseInt(raw, 10);
    return Number.isFinite(parsed) ? parsed : null;
  }
}
