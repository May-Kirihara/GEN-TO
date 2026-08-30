/**
 * クイックプレビューのストア (MVP WP-4 / 仕様§8)。
 *
 * - Spaceで現在セルを中央拡大オーバーレイ表示。再Space/Escで閉じる、
 *   矢印で前後の項目へ移動 (グリッドの選択フォーカスと同期するのはApp側)。
 * - 表示はpreview2048 (ビューアと同じ資産)。未キャッシュ時は
 *   request_thumbnails(interactive) → thumb:ready でURL更新。
 *   アニメーション画像はoriginalを直接表示 (WP-3のビューア方針に同じ)。
 * - ReadyMap (grid/demand) をpreview2048用に流用 (WP-5パターン)。
 */

import type { ImageListItem } from "../catalog-types";
import type { ThumbFailedItem, ThumbReadyItem } from "../thumbnail-types";
import { originalUrl, previewUrl } from "../protocol";
import { ReadyMap } from "../grid/demand";

export type PreviewIpc = {
  invoke: <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
  listen: <T>(
    event: string,
    handler: (event: { payload: T }) => void,
  ) => Promise<() => void>;
};

/** カタログ項目の供給源 (GridStoreのpagerを想定。ビューアと同一形状)。 */
export type PreviewItems = {
  total(): number;
  get(index: number): ImageListItem | null;
  ensureRange(start: number, end: number): Promise<void>;
};

const PREVIEW_CLASS = "preview2048";

function toMessage(value: unknown): string {
  return value instanceof Error ? value.message : String(value);
}

export class QuickPreviewStore {
  active = false;
  index = 0;
  url: string | null = null;
  /** 表示中の項目メタ (キャプション表示用)。 */
  item: ImageListItem | null = null;
  loading = false;
  lastError: string | null = null;
  version = 0;

  readonly ready = new ReadyMap();

  private readonly unlisteners: Array<() => void> = [];
  private readonly changeListeners = new Set<() => void>();
  private pending = new Set<string>();
  private disposed = false;

  constructor(
    private readonly ipc: PreviewIpc,
    private readonly items: PreviewItems,
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
    }
  }

  /** thumb:ready / thumb:failed の購読。dispose関数を返す。 */
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
    return () => this.dispose();
  }

  dispose(): void {
    this.disposed = true;
    for (const unlisten of this.unlisteners.splice(0)) {
      unlisten();
    }
    this.changeListeners.clear();
  }

  /** indexで開く (項目解決を待つ)。 */
  async open(index: number): Promise<void> {
    this.active = true;
    await this.show(index);
  }

  /** 前後へ移動 (端では移動しない)。 */
  async move(delta: number): Promise<void> {
    if (!this.active || delta === 0) {
      return;
    }
    const total = this.items.total();
    const next = Math.min(total - 1, Math.max(0, this.index + delta));
    if (next === this.index) {
      return;
    }
    await this.show(next);
  }

  /** 供給源の総件数 (表示用)。 */
  total(): number {
    return this.items.total();
  }

  close(): void {
    this.active = false;
    this.url = null;
    this.item = null;
    this.loading = false;
    this.notify();
  }

  private async show(index: number): Promise<void> {
    this.index = index;
    this.url = null;
    this.item = null;
    this.loading = true;
    this.notify();
    await this.items.ensureRange(index, index).catch(() => {});
    if (this.disposed || !this.active || this.index !== index) {
      return;
    }
    const item = this.items.get(index);
    if (item === null) {
      this.loading = false;
      this.lastError = "項目を取得できませんでした";
      this.notify();
      return;
    }
    this.item = item;
    if (item.state !== "ready") {
      this.loading = false;
      this.notify();
      return;
    }
    if (item.isAnimated) {
      // アニメーションはoriginal直接表示 (WP-3方針)
      this.url = originalUrl(item.id, item.version || 1);
      this.loading = false;
      this.notify();
      return;
    }
    const readyVersion = this.ready.version(PREVIEW_CLASS, item.id);
    if (readyVersion !== null) {
      this.url = previewUrl(item.id, readyVersion);
      this.loading = false;
      this.notify();
      return;
    }
    await this.requestPreview(item.id);
    // ready到着でapplyReadyがURLを埋める (loadingはそこで解消)
  }

  private async requestPreview(id: string): Promise<void> {
    const key = `${PREVIEW_CLASS}:${id}`;
    if (this.pending.has(key)) {
      return;
    }
    this.pending.add(key);
    try {
      await this.ipc.invoke("request_thumbnails", {
        ids: [id],
        sizeClass: PREVIEW_CLASS,
        priority: "interactive",
      });
    } catch (error) {
      this.pending.delete(key);
      this.lastError = toMessage(error);
      this.notify();
    }
  }

  private applyReady(items: ThumbReadyItem[]): void {
    const accepted = this.ready.applyReady(items);
    if (accepted === 0) {
      return;
    }
    const cur = this.item;
    for (const entry of items) {
      this.pending.delete(`${entry.size_class}:${entry.asset_id}`);
      if (
        cur !== null &&
        cur.id === entry.asset_id &&
        entry.size_class === PREVIEW_CLASS &&
        this.active &&
        this.url === null
      ) {
        this.url = previewUrl(entry.asset_id, entry.version);
        this.loading = false;
      }
    }
    this.notify();
  }

  private applyFailed(items: ThumbFailedItem[]): void {
    const cur = this.item;
    for (const entry of items) {
      this.pending.delete(`${entry.size_class}:${entry.asset_id}`);
      if (cur !== null && cur.id === entry.asset_id && entry.size_class === PREVIEW_CLASS) {
        this.loading = false;
        this.lastError = "プレビューの生成に失敗しました";
      }
    }
    this.notify();
  }
}
