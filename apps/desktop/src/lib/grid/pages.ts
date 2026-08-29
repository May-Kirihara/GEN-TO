/**
 * カタログページングのキャッシュ層 (WP-5)。
 *
 * - 表示index範囲に必要なページを前後1ページ余裕で取得
 * - ページはoffset整列 (pageNo * pageSize) でキャッシュ
 * - staleGenerationはsummary再取得→1回再試行 (CatalogStoreと同じ規約)
 * - generation変化でキャッシュクリア (onResetでGridStoreへ通知)
 */
import type { CatalogPage, CatalogSummary, ImageListItem } from "../catalog-types";
import { isStaleGenerationError } from "../catalog";

/** index範囲 (endは含む) → ページ番号リスト (前後marginページ、クランプ付き)。 */
export function pagesForIndexRange(
  start: number,
  end: number,
  total: number,
  pageSize: number,
  margin = 1,
): number[] {
  if (total <= 0 || pageSize <= 0 || start > end || start < 0) {
    return [];
  }
  const lastPage = Math.floor((total - 1) / pageSize);
  const first = Math.max(0, Math.floor(start / pageSize) - margin);
  const last = Math.min(lastPage, Math.floor(end / pageSize) + margin);
  const pages: number[] = [];
  for (let page = first; page <= last; page++) {
    pages.push(page);
  }
  return pages;
}

export type PagerIpc = {
  invoke: <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
};

function toMessage(value: unknown): string {
  return value instanceof Error ? value.message : String(value);
}

/**
 * invokeにタイムアウトを付ける。WebViewのフォーカス遷移等でIPC応答が
 * 喪失した場合、promiseが永久に未解決となりensureRangeの待機が
 * デッドロックするのを防ぐ (タイムアウト時はエラー扱いで再試行可能)。
 */
function withTimeout<T>(promise: Promise<T>, ms: number, what: string): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => {
      reject(new Error(`${what} timed out after ${ms}ms`));
    }, ms);
    promise.then(
      (value) => {
        clearTimeout(timer);
        resolve(value);
      },
      (error) => {
        clearTimeout(timer);
        reject(error);
      },
    );
  });
}

export class CatalogPager {
  generation: number | null = null;
  total = 0;
  stats = { fetched: 0, errors: 0 };
  lastError: string | null = null;
  /** generationが変わった時に呼ばれる (ready地図クリア等)。 */
  onReset: ((generation: number) => void) | null = null;

  private readonly pages = new Map<number, ImageListItem[]>();
  private readonly inflight = new Map<number, Promise<void>>();
  private readonly pageSize: number;
  private readonly timeoutMs: number;

  constructor(
    private readonly ipc: PagerIpc,
    opts?: { pageSize?: number; timeoutMs?: number },
  ) {
    this.pageSize = Math.min(opts?.pageSize ?? 200, 500);
    this.timeoutMs = opts?.timeoutMs ?? 8000;
  }

  /** generation/totalの再同期。generation変化でキャッシュクリア。 */
  reset(generation: number, total: number): void {
    if (generation !== this.generation) {
      this.pages.clear();
      this.generation = generation;
      this.onReset?.(generation);
    }
    this.total = total;
  }

  hasPage(page: number): boolean {
    return this.pages.has(page);
  }

  /**
   * キャッシュ済みページのみを走査して id→index を解決する。
   * 見つからないidは結果に含めない (呼び出し側は連続補充へフォールバック)。
   * 比較表示の選択充填 (WP-6) 用 — 全カタログ走査は行わない。
   */
  findIndicesByIds(ids: ReadonlySet<string>): Map<string, number> {
    const found = new Map<string, number>();
    const remaining = new Set(ids);
    if (remaining.size === 0 || this.pageSize <= 0) {
      return found;
    }
    const pageNumbers = [...this.pages.keys()].sort((a, b) => a - b);
    for (const page of pageNumbers) {
      const items = this.pages.get(page);
      if (!items) {
        continue;
      }
      for (let i = 0; i < items.length; i++) {
        const id = items[i].id;
        if (remaining.delete(id)) {
          found.set(id, page * this.pageSize + i);
          if (remaining.size === 0) {
            return found;
          }
        }
      }
    }
    return found;
  }

  getItem(index: number): ImageListItem | null {
    if (index < 0 || index >= this.total) {
      return null;
    }
    const page = Math.floor(index / this.pageSize);
    return this.pages.get(page)?.[index - page * this.pageSize] ?? null;
  }

  /** 表示index範囲 (endは含む) に必要なページを取得する (並行呼び出しは1回にdedup)。 */
  async ensureRange(start: number, end: number): Promise<void> {
    if (this.generation === null) {
      return;
    }
    const targets = pagesForIndexRange(start, end, this.total, this.pageSize);
    const waits: Promise<void>[] = [];
    for (const page of targets) {
      if (this.pages.has(page) || this.inflight.has(page)) {
        const inflight = this.inflight.get(page);
        if (inflight) {
          waits.push(inflight);
        }
        continue;
      }
      const promise = this.fetchPage(page).finally(() => {
        this.inflight.delete(page);
      });
      this.inflight.set(page, promise);
      waits.push(promise);
    }
    await Promise.all(waits);
  }

  private async fetchPage(page: number): Promise<void> {
    if (this.generation === null) {
      return;
    }
    const offset = page * this.pageSize;
    try {
      const { result, generation } = await this.fetchPageWithRetry(offset);
      // 取得中にgenerationが進んだ場合は破棄 (新しいカタログの内容と混ぜない)
      if (generation === this.generation) {
        this.lastError = null;
        this.pages.set(page, result.items);
        this.stats.fetched++;
      }
    } catch (error) {
      this.stats.errors++;
      this.lastError = toMessage(error);
    }
  }

  private async fetchPageWithRetry(
    offset: number,
  ): Promise<{ result: CatalogPage; generation: number }> {
    const generation = this.generation;
    if (generation === null) {
      throw new Error("pager not initialized");
    }
    try {
      const result = await withTimeout(
        this.ipc.invoke<CatalogPage>("get_catalog_page", {
          generation,
          offset,
          limit: this.pageSize,
        }),
        this.timeoutMs,
        "get_catalog_page",
      );
      return { result, generation };
    } catch (error) {
      if (!isStaleGenerationError(error)) {
        throw error;
      }
      // stale: summaryを引き直して新しいgenerationで1回だけ再試行
      const summary = await withTimeout(
        this.ipc.invoke<CatalogSummary>("get_catalog_summary"),
        this.timeoutMs,
        "get_catalog_summary",
      );
      if (summary.generation !== generation) {
        this.reset(summary.generation, summary.total);
      } else {
        this.total = summary.total;
      }
      const result = await withTimeout(
        this.ipc.invoke<CatalogPage>("get_catalog_page", {
          generation: summary.generation,
          offset,
          limit: this.pageSize,
        }),
        this.timeoutMs,
        "get_catalog_page (retry)",
      );
      return { result, generation: summary.generation };
    }
  }
}
