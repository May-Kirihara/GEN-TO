/**
 * サムネイル生成 (WP-3) のフロントエンドストア。
 *
 * - 統計を1秒ポーリングして開発パネルへ表示
 * - 全件生成 / 一時停止・再開 / キャッシュ全削除 のボタン操作
 * - thumb:ready / thumb:failed イベントの購読 (WP-3ではログ表示のみ。
 *   表示本体はWP-4/5)
 *
 * IPC境界は注入 (ThumbnailIpc) してテスト容易にする (CatalogStoreと同じ構成)。
 */
import type { RequestAccepted, ThumbnailStats } from "./thumbnail-types";

export type { RequestAccepted, ThumbnailStats } from "./thumbnail-types";

export type ThumbnailIpc = {
  invoke: <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
  listen: <T>(
    event: string,
    handler: (event: { payload: T }) => void,
  ) => Promise<() => void>;
};

/** イベントログの最大行数 (リングバッファ)。 */
export const EVENT_LOG_MAX = 30;

function toMessage(value: unknown): string {
  return value instanceof Error ? value.message : String(value);
}

/** ヒット率の表示書式。データなしは「—」。 */
export function formatHitRate(rate: number | null): string {
  if (rate === null) {
    return "—";
  }
  return `${(rate * 100).toFixed(1)}%`;
}

/** ミリ秒の表示書式 (1秒以上は秒表記)。 */
export function formatMs(ms: number | null): string {
  if (ms === null) {
    return "—";
  }
  if (ms >= 1000) {
    return `${(ms / 1000).toFixed(2)} s`;
  }
  return `${Math.round(ms)} ms`;
}

/** バイト数の表示書式。 */
export function formatBytes(bytes: number): string {
  if (bytes >= 1024 * 1024 * 1024) {
    return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GiB`;
  }
  if (bytes >= 1024 * 1024) {
    return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
  }
  if (bytes >= 1024) {
    return `${(bytes / 1024).toFixed(0)} KiB`;
  }
  return `${bytes} B`;
}

/** サムネイル開発パネル用ストア。 */
export class ThumbnailStore {
  stats: ThumbnailStats | null = null;
  lastError: string | null = null;
  lastNotice = "";
  /** 直近のイベントログ行 (新しい順に先頭)。 */
  eventLog: string[] = [];
  /** 受信した ready / failed の累計件数 (検証用)。 */
  readyCount = 0;
  failedCount = 0;

  private readonly unlisteners: Array<() => void> = [];
  private readonly changeListeners = new Set<() => void>();
  private pollTimer: ReturnType<typeof setInterval> | null = null;
  private disposed = false;

  constructor(
    private readonly ipc: ThumbnailIpc,
    private readonly pollMs = 1000,
  ) {}

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

  /** イベント購読とポーリングを開始し、dispose関数を返す。 */
  async init(): Promise<() => void> {
    const unlistenReady = await this.ipc.listen<{
      items: Array<{ asset_id: string; size_class: string; cache_generation: number }>;
    }>("thumb:ready", (event) => {
      this.applyReady(event.payload.items);
    });
    const unlistenFailed = await this.ipc.listen<{
      items: Array<{ asset_id: string; size_class: string; reason: string }>;
    }>("thumb:failed", (event) => {
      this.applyFailed(event.payload.items);
    });
    this.unlisteners.push(unlistenReady, unlistenFailed);
    await this.refreshStats();
    this.startPolling();
    return () => this.dispose();
  }

  private startPolling(): void {
    if (this.pollTimer !== null || this.disposed) {
      return;
    }
    this.pollTimer = setInterval(() => {
      void this.refreshStats();
    }, this.pollMs);
  }

  dispose(): void {
    this.disposed = true;
    if (this.pollTimer !== null) {
      clearInterval(this.pollTimer);
      this.pollTimer = null;
    }
    for (const unlisten of this.unlisteners.splice(0)) {
      unlisten();
    }
  }

  async refreshStats(): Promise<void> {
    try {
      this.stats = await this.ipc.invoke<ThumbnailStats>("get_thumbnail_stats");
      this.lastError = null;
    } catch (error) {
      this.lastError = toMessage(error);
    }
    this.notify();
  }

  /** 全カタログをBackground優先度で一括登録。 */
  async generateAll(sizeClass: string): Promise<boolean> {
    try {
      const accepted = await this.ipc.invoke<RequestAccepted>(
        "generate_all_thumbnails",
        { sizeClass },
      );
      this.lastError = null;
      this.lastNotice =
        `受付: キュー ${accepted.queued} / 重複 ${accepted.rejected_dup}` +
        ` / ヒット ${accepted.cache_hits} / 読込不能 ${accepted.skipped_unreadable}` +
        (accepted.rejected_full > 0 ? ` / 満杯拒否 ${accepted.rejected_full}` : "");
      await this.refreshStats();
      return true;
    } catch (error) {
      this.lastError = toMessage(error);
      this.notify();
      return false;
    }
  }

  async setPaused(paused: boolean): Promise<boolean> {
    try {
      await this.ipc.invoke("pause_thumbnail_generation", { paused });
      this.lastError = null;
      await this.refreshStats();
      return true;
    } catch (error) {
      this.lastError = toMessage(error);
      this.notify();
      return false;
    }
  }

  async clearCache(): Promise<boolean> {
    try {
      const result = await this.ipc.invoke<{
        removed_files: number;
        removed_bytes: number;
        cache_generation: number;
      }>("clear_thumbnail_cache");
      this.lastError = null;
      this.lastNotice =
        `削除 ${result.removed_files} 件 / ${formatBytes(result.removed_bytes)}` +
        ` (generation → ${result.cache_generation})`;
      await this.refreshStats();
      return true;
    } catch (error) {
      this.lastError = toMessage(error);
      this.notify();
      return false;
    }
  }

  private applyReady(items: Array<{ asset_id: string }>): void {
    this.readyCount += items.length;
    this.pushLog(`ready: ${items.length} 件 (例: ${items[0]?.asset_id ?? "?"})`);
    this.notify();
  }

  private applyFailed(items: Array<{ asset_id: string; reason: string }>): void {
    this.failedCount += items.length;
    this.pushLog(`failed: ${items[0]?.asset_id ?? "?"} — ${items[0]?.reason ?? "?"}`);
    this.notify();
  }

  private pushLog(line: string): void {
    const stamped = `${new Date().toLocaleTimeString()} ${line}`;
    this.eventLog.unshift(stamped);
    if (this.eventLog.length > EVENT_LOG_MAX) {
      this.eventLog.length = EVENT_LOG_MAX;
    }
  }
}
