/**
 * 仮想サムネイルグリッドのグルーストア (WP-5)。
 *
 * - CatalogPager: 表示レンジ→ページ取得 (前後1ページ余裕)
 * - 需要スケジューラ: 可視レンジ→未生成ID抽出→デバウンス (150ms) →
 *   優先度別バッチで request_thumbnails 発行
 * - thumb:ready / thumb:failed → ready地図・失敗地図更新 (世代ガード付き)
 * - rejected_full は再試行せず次のレンジ変化に委ねる (統計のみ記録)
 *
 * Svelte 5の `$state` はクラスインスタンスを深くプロキシしないため、
 * UI側は onChange でローカル $state 変数へ同期する (WP-2の教訓)。
 */
import type { CatalogSummary, ImageListItem } from "../catalog-types";
import type {
  CancelThumbnailResult,
  RequestAccepted,
  ThumbFailedItem,
  ThumbReadyItem,
} from "../thumbnail-types";
import { thumbUrl, type ThumbSizeClass } from "../protocol";
import { CatalogPager } from "./pages";
import { totalRows } from "./layout";
import {
  FailureSet,
  ReadyMap,
  directionFromDelta,
  keepZoneRows,
  pendingKey,
  planDemand,
  type RowRange,
} from "./demand";

export type GridIpc = {
  invoke: <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
  listen: <T>(
    event: string,
    handler: (event: { payload: T }) => void,
  ) => Promise<() => void>;
};

export type GridStats = {
  /** 要求へ送出したID数の合計。 */
  requested: number;
  /** request_thumbnails 呼び出し回数。 */
  batches: number;
  /** rejected_full で拒否されたID数 (バックプレッシャー)。 */
  rejectedFull: number;
  /** 受け入れた thumb:ready 項目数。 */
  readyReceived: number;
  /** thumb:failed 項目数。 */
  failedReceived: number;
  /** 画面外キャンセルしたID数 (WP-7)。 */
  cancelled: number;
  /** cancel_thumbnail_requests 呼び出し回数 (WP-7)。 */
  cancelBatches: number;
};

function toMessage(value: unknown): string {
  return value instanceof Error ? value.message : String(value);
}

/**
 * invokeにタイムアウトを付ける。IPC応答喪失時にpendingが永久に残り
 * 以降の需要計画から除外され続けるのを防ぐ。
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

export class GridStore {
  summary: CatalogSummary | null = null;
  stats: GridStats = {
    requested: 0,
    batches: 0,
    rejectedFull: 0,
    readyReceived: 0,
    failedReceived: 0,
    cancelled: 0,
    cancelBatches: 0,
  };
  lastError: string | null = null;
  /** ready/failed/ページ到着で増える版番号 (セル再描画のトリガー用)。 */
  version = 0;

  readonly pager: CatalogPager;

  private readonly changeListeners = new Set<() => void>();
  private readonly ready = new ReadyMap();
  private readonly failures = new FailureSet();
  private pending = new Set<string>();
  private sizeClassValue: ThumbSizeClass = "s256";
  private debounceTimer: ReturnType<typeof setTimeout> | null = null;
  private pendingViewport: {
    range: RowRange;
    overscanRows: number;
    columns: number;
  } | null = null;
  private lastScrollTop = 0;
  private direction: 1 | -1 | 0 = 0;
  private disposed = false;
  private readonly unlisteners: Array<() => void> = [];
  private readonly debounceMs: number;
  private readonly prefetchRows: number;
  private readonly maxBatch: number;
  private readonly requestTimeoutMs: number;

  constructor(
    private readonly ipc: GridIpc,
    opts?: {
      pageSize?: number;
      debounceMs?: number;
      prefetchRows?: number;
      maxBatch?: number;
      requestTimeoutMs?: number;
    },
  ) {
    this.debounceMs = opts?.debounceMs ?? 150;
    this.prefetchRows = opts?.prefetchRows ?? 4;
    this.maxBatch = opts?.maxBatch ?? 100;
    this.requestTimeoutMs = opts?.requestTimeoutMs ?? 8000;
    this.pager = new CatalogPager(ipc, { pageSize: opts?.pageSize });
    this.pager.onReset = () => {
      this.ready.clear();
      this.failures.clear();
      this.pending.clear();
      this.version++;
    };
  }

  onChange(listener: () => void): () => void {
    this.changeListeners.add(listener);
    return () => {
      this.changeListeners.delete(listener);
    };
  }

  private notify(): void {
    for (const listener of this.changeListeners) {
      listener();
    }
  }

  get sizeClass(): ThumbSizeClass {
    return this.sizeClassValue;
  }

  /** イベント購読とsummary初回取得。dispose関数を返す。 */
  async init(): Promise<() => void> {
    const unlistenReady = await this.ipc.listen<{ items: ThumbReadyItem[] }>(
      "thumb:ready",
      (event) => {
        this.applyReady(event.payload.items);
      },
    );
    const unlistenFailed = await this.ipc.listen<{ items: ThumbFailedItem[] }>(
      "thumb:failed",
      (event) => {
        this.applyFailed(event.payload.items);
      },
    );
    this.unlisteners.push(unlistenReady, unlistenFailed);
    await this.refreshSummary();
    return () => this.dispose();
  }

  private dispose(): void {
    this.disposed = true;
    if (this.debounceTimer !== null) {
      clearTimeout(this.debounceTimer);
      this.debounceTimer = null;
    }
    for (const unlisten of this.unlisteners.splice(0)) {
      unlisten();
    }
  }

  private async refreshSummary(): Promise<void> {
    try {
      const summary = await this.ipc.invoke<CatalogSummary>("get_catalog_summary");
      this.syncSummary(summary);
    } catch (error) {
      this.lastError = toMessage(error);
      this.notify();
    }
  }

  /** App側のCatalogStoreと同じsummaryを共有する (generation変化で関連状態を初期化)。 */
  syncSummary(summary: CatalogSummary): void {
    this.summary = summary;
    this.pager.reset(summary.generation, summary.total);
    this.notify();
  }

  setSizeClass(sizeClass: ThumbSizeClass): void {
    if (this.sizeClassValue !== sizeClass) {
      this.sizeClassValue = sizeClass;
      this.notify();
    }
  }

  /** キャッシュ全削除の通知 (clear_thumbnail_cache の応答generation)。 */
  noteCacheCleared(cacheGeneration: number): void {
    this.ready.noteGeneration(cacheGeneration);
    this.version++;
    this.notify();
  }

  getItem(index: number) {
    return this.pager.getItem(index);
  }

  /**
   * `asset:updated` 由来のローカル項目パッチ (WP-5)。
   * キャッシュ済みページの該当項目を書き換え、版を進めてセル再描画させる。
   */
  applyItemPatch(
    ids: ReadonlySet<string>,
    patch: Partial<Pick<ImageListItem, "rating" | "triage" | "favorite">>,
  ): void {
    const patched = this.pager.patchItems(ids, patch);
    if (patched > 0) {
      this.version++;
      this.notify();
    }
  }

  /** ready地図からプロトコルURLを構成する (未生成はnull)。 */
  thumbUrlFor(id: string): string | null {
    const version = this.ready.version(this.sizeClassValue, id);
    return version === null ? null : thumbUrl(id, this.sizeClassValue, version);
  }

  /** このセルが現在のサイズクラスで生成失敗しているか (WP-7 A2の失敗表示)。 */
  isFailed(id: string): boolean {
    return this.failures.has(this.sizeClassValue, id);
  }

  /**
   * ビューポート変化の通知。rangeは**行番号** (virtualizer準拠)。
   * ページ取得は即時、サムネイル要求はデバウンスする。
   * 方向はscrollTop差分から判定 (閾値未満は現行維持)。
   */
  viewportChanged(range: RowRange | null, overscanRows: number, scrollTop: number, columns: number): void {
    const nextDirection = directionFromDelta(this.lastScrollTop, scrollTop);
    if (nextDirection !== 0) {
      this.direction = nextDirection;
    }
    this.lastScrollTop = scrollTop;
    this.pendingViewport = range === null ? null : { range, overscanRows, columns };
    if (range !== null) {
      const indexRange = this.rowRangeToIndices(range, columns);
      void this.pager.ensureRange(indexRange.start, indexRange.end).then(() => {
        this.notify();
      });
    }
    if (this.debounceTimer === null) {
      this.debounceTimer = setTimeout(() => {
        this.debounceTimer = null;
        void this.fireDemand();
      }, this.debounceMs);
    }
  }

  /** 行範囲 → アイテムindex範囲 (endは含む。total未満へクランプ)。 */
  private rowRangeToIndices(range: RowRange, columns: number): { start: number; end: number } {
    if (columns <= 0) {
      return { start: 0, end: -1 };
    }
    const start = Math.max(0, range.start) * columns;
    const end = Math.min(this.pager.total - 1, (range.end + 1) * columns - 1);
    return { start, end };
  }

  private async fireDemand(): Promise<void> {
    if (this.disposed || this.summary === null || this.pager.generation === null) {
      return;
    }
    const snapshot = this.pendingViewport;
    if (snapshot === null) {
      return;
    }
    const indexRange = this.rowRangeToIndices(snapshot.range, snapshot.columns);
    await this.pager.ensureRange(indexRange.start, indexRange.end);
    const latest = this.pendingViewport;
    if (latest === null || this.summary === null) {
      return;
    }
    // 画面外へ出た未開始要求のキャンセルと、画面外失敗記録のリトラクト (WP-7)。
    // 新しい需要を出す前に実行し「過去の要求を長時間処理し続けない」を保証する。
    await this.reapOutsideKeepZone(latest);
    const total = this.pager.total;
    const rowCount = totalRows(total, latest.columns);
    if (rowCount === 0) {
      return;
    }
    const plan = planDemand({
      visible: latest.range,
      overscanBefore: latest.overscanRows,
      overscanAfter: latest.overscanRows,
      direction: this.direction,
      totalRows: rowCount,
      columns: latest.columns,
      total,
      prefetchRows: this.prefetchRows,
      maxBatch: this.maxBatch,
      sizeClass: this.sizeClassValue,
      ready: this.ready,
      failures: this.failures,
      pending: this.pending,
      itemAt: (index) => {
        const item = this.pager.getItem(index);
        return item === null ? null : { id: item.id, state: item.state };
      },
    });
    for (const batch of plan) {
      for (const id of batch.ids) {
        this.pending.add(pendingKey(this.sizeClassValue, id));
      }
      this.stats.batches++;
      this.stats.requested += batch.ids.length;
      try {
        const accepted = await withTimeout(
          this.ipc.invoke<RequestAccepted>("request_thumbnails", {
            ids: batch.ids,
            sizeClass: this.sizeClassValue,
            priority: batch.priority,
          }),
          this.requestTimeoutMs,
          "request_thumbnails",
        );
        if (accepted.rejected_full > 0) {
          // 再試行せず次のレンジ変化に委ねる (ログ/統計のみ)
          this.stats.rejectedFull += accepted.rejected_full;
          for (const id of batch.ids) {
            this.pending.delete(pendingKey(this.sizeClassValue, id));
          }
        }
      } catch (error) {
        this.lastError = toMessage(error);
        for (const id of batch.ids) {
          this.pending.delete(pendingKey(this.sizeClassValue, id));
        }
      }
    }
  }

  /**
   * 維持ゾーン (visible ∪ prefetch ∪ overscan) の外へ出た要求・失敗記録を
   * 掃除する (WP-7 / フェーズ8)。
   *
   * - pending内の現在サイズクラスのIDで、インデックス解決できたもののうち
   *   ゾーン外のものを cancel_thumbnail_requests へ送る
   *   (未開始ジョブは即時除去。進行方向先読み行はゾーンに含むため維持)
   * - 失敗記録はゾーン外へ出たら解除する (再登場時に次のレンジ変化で再試行)
   * - インデックス解決不能 (ページ未取得) のIDは保守的に残す
   */
  private async reapOutsideKeepZone(latest: {
    range: RowRange;
    overscanRows: number;
    columns: number;
  }): Promise<void> {
    const sizeClass = this.sizeClassValue;
    const zone = keepZoneRows({
      visible: latest.range,
      overscanBefore: latest.overscanRows,
      overscanAfter: latest.overscanRows,
      direction: this.direction,
      prefetchRows: this.prefetchRows,
    });
    const lastRow = totalRows(this.pager.total, latest.columns) - 1;
    const zoneStart = Math.max(0, zone.start) * latest.columns;
    const zoneEnd = Math.min(lastRow, zone.end + 1) * latest.columns - 1;

    const pendingIds: string[] = [];
    for (const key of this.pending) {
      const sep = key.indexOf(":");
      if (sep >= 0 && key.slice(0, sep) === sizeClass) {
        pendingIds.push(key.slice(sep + 1));
      }
    }
    const failedIds = this.failures.ids(sizeClass);
    if (pendingIds.length === 0 && failedIds.length === 0) {
      return;
    }

    const resolve = new Map<string, number>();
    const wanted = new Set<string>([...pendingIds, ...failedIds]);
    for (const [id, index] of this.pager.findIndicesByIds(wanted)) {
      resolve.set(id, index);
    }

    const outside = (id: string): boolean => {
      const index = resolve.get(id);
      return index !== undefined && (index < zoneStart || index > zoneEnd);
    };

    const stalePending = pendingIds.filter(outside);
    if (stalePending.length > 0) {
      this.stats.cancelBatches++;
      try {
        const result = await withTimeout(
          this.ipc.invoke<CancelThumbnailResult>("cancel_thumbnail_requests", {
            ids: stalePending,
            sizeClass,
          }),
          this.requestTimeoutMs,
          "cancel_thumbnail_requests",
        );
        this.stats.cancelled += result.cancelled_queued + result.cancel_requested;
      } catch (error) {
        this.lastError = toMessage(error);
      }
      // 要求済みジョブの成否はthumb:ready/failedで追跡するため、
      // キャンセル結果にかかわらずpendingからは外す (再登場時の再要求を許す)
      for (const id of stalePending) {
        this.pending.delete(pendingKey(sizeClass, id));
      }
    }

    const staleFailed = failedIds.filter(outside);
    if (staleFailed.length > 0) {
      this.failures.retract(sizeClass, staleFailed);
    }
  }

  private applyReady(items: ThumbReadyItem[]): void {
    const accepted = this.ready.applyReady(items);
    this.stats.readyReceived += accepted;
    for (const item of items) {
      if (this.ready.has(item.size_class, item.asset_id)) {
        this.pending.delete(pendingKey(item.size_class, item.asset_id));
      }
    }
    this.version++;
    this.notify();
  }

  private applyFailed(items: ThumbFailedItem[]): void {
    for (const item of items) {
      this.failures.add(item.size_class, [item.asset_id]);
      this.pending.delete(pendingKey(item.size_class, item.asset_id));
    }
    this.stats.failedReceived += items.length;
    this.version++;
    this.notify();
  }
}
