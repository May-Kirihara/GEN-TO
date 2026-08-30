/**
 * Rustカタログcommand群 (WP-2) のフロントエンドストア。
 *
 * - command呼び出し・イベント購読を注入 (CatalogIpc) してテスト容易にする
 * - generation管理: ページ要求時のgenerationと応答のgeneration比較で
 *   古い応答を破棄する (docs/dev/api-contract.md「generation semantics」)
 * - staleGenerationエラー時は summary再取得 → 同一オフセットで1回再試行
 * - 型契約は docs/dev/api-contract.md (DTO camelCase、summary/event snake_case)
 */
import type {
  CatalogPage,
  CatalogSort,
  CatalogSummary,
  ScanProgressEvent,
} from "./catalog-types";

export type {
  CatalogPage,
  CatalogSort,
  CatalogSummary,
  ImageListItem,
  ScanProgressEvent,
} from "./catalog-types";

/** Tauri IPC境界の注入用アダプター (本物は@tauri-apps/api、試験はモック)。 */
export type CatalogIpc = {
  invoke: <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
  listen: <T>(
    event: string,
    handler: (event: { payload: T }) => void,
  ) => Promise<() => void>;
};

export function isStaleGenerationError(
  value: unknown,
): value is { kind: "staleGeneration"; requested: number; current: number } {
  return (
    typeof value === "object" &&
    value !== null &&
    (value as Record<string, unknown>)["kind"] === "staleGeneration"
  );
}

function toMessage(value: unknown): string {
  return value instanceof Error ? value.message : String(value);
}

/** カタログUI状態ストア。
 *
 * Svelte 5の `$state` はクラスインスタンスを深くプロキシしないため、
 * UI側は `onChange` で変更通知を受けてローカル `$state` 変数へ同期すること
 * (App.svelte参照)。
 */
export class CatalogStore {
  summary: CatalogSummary | null = null;
  progress: ScanProgressEvent | null = null;
  pageItems: Array<CatalogPage["items"][number]> = [];
  pageOffset = 0;
  pageHasMore = false;
  lastError: string | null = null;
  /** open_folder の失敗文言 (不在・権限等)。グリッド空状態の分岐に使う (WP-4)。 */
  folderError: string | null = null;

  private readonly unlisteners: Array<() => void> = [];
  private readonly changeListeners = new Set<() => void>();
  /** 発行順を追跡し、古いページ応答を破棄するための連番。 */
  private requestSeq = 0;
  private readonly pageSize: number;

  constructor(
    private readonly ipc: CatalogIpc,
    pageSize = 200,
  ) {
    this.pageSize = Math.min(pageSize, 500);
  }

  /** 状態変更通知の購読。解除関数を返す。 */
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

  /** summary取得・イベント購読・先頭ページ取得を行い、dispose関数を返す。 */
  async init(): Promise<() => void> {
    await this.refreshSummary();
    const unlistenProgress = await this.ipc.listen<ScanProgressEvent>(
      "scan:progress",
      (event) => {
        this.applyProgress(event.payload);
      },
    );
    const unlistenChanged = await this.ipc.listen(
      "catalog:changed",
      (event: { payload: { generation: number; reason: string } }) => {
        void this.applyChanged(event.payload);
      },
    );
    this.unlisteners.push(unlistenProgress, unlistenChanged);
    await this.loadPage(0);
    return () => {
      for (const unlisten of this.unlisteners.splice(0)) {
        unlisten();
      }
    };
  }

  /** スキャン開始。成功時はsummaryを再取得する (startでgenerationが変わる)。 */
  async startScan(): Promise<boolean> {
    try {
      await this.ipc.invoke("start_scan");
      this.lastError = null;
      await this.refreshSummary();
      return true;
    } catch (error) {
      this.lastError = toMessage(error);
      this.notify();
      return false;
    }
  }

  async refreshSummary(): Promise<boolean> {
    try {
      this.summary = await this.ipc.invoke<CatalogSummary>("get_catalog_summary");
      return true;
    } catch (error) {
      this.lastError = toMessage(error);
      return false;
    } finally {
      this.notify();
    }
  }

  /** ページ取得。staleGeneration時はsummary再取得→1回だけ再試行する。 */
  async loadPage(offset: number): Promise<void> {
    if (this.summary === null) {
      const refreshed = await this.refreshSummary();
      if (!refreshed || this.summary === null) {
        return;
      }
    }
    const generation = this.summary.generation;
    const seq = ++this.requestSeq;
    try {
      const page = await this.ipc.invoke<CatalogPage>("get_catalog_page", {
        generation,
        offset,
        limit: this.pageSize,
      });
      if (seq !== this.requestSeq || page.generation !== generation) {
        return;
      }
      this.lastError = null;
      this.applyPage(page);
    } catch (error) {
      if (!isStaleGenerationError(error)) {
        if (seq === this.requestSeq) {
          this.lastError = toMessage(error);
          this.notify();
        }
        return;
      }
      // stale: summaryを引き直して新しいgenerationで再試行
      const refreshed = await this.refreshSummary();
      if (seq !== this.requestSeq || !refreshed || this.summary === null) {
        return;
      }
      const retryGeneration = this.summary.generation;
      try {
        const page = await this.ipc.invoke<CatalogPage>("get_catalog_page", {
          generation: retryGeneration,
          offset,
          limit: this.pageSize,
        });
        if (seq !== this.requestSeq || page.generation !== retryGeneration) {
          return;
        }
        this.applyPage(page);
        this.lastError = null;
        this.notify();
      } catch (retryError) {
        if (seq === this.requestSeq) {
          this.lastError = toMessage(retryError);
          this.notify();
        }
      }
    }
  }

  /** カタログモード切替。成功時は新summaryを適用し先頭ページを再取得する。 */
  async setMode(virtual: boolean): Promise<boolean> {
    try {
      this.summary = await this.ipc.invoke<CatalogSummary>("set_catalog_mode", { virtual });
      this.lastError = null;
      await this.loadPage(0);
      return true;
    } catch (error) {
      this.lastError = toMessage(error);
      this.notify();
      return false;
    }
  }

  /**
   * 閲覧フォルダを切り替える (WP-4。ツリー選択)。
   * 成功時は走査開始後のsummaryを受け取り、`folderError` を解消する。
   * 失敗時 (不在・権限・走査中) は folderError へ文言を設定する。
   */
  async openFolder(path: string): Promise<boolean> {
    this.folderError = null;
    try {
      this.summary = await this.ipc.invoke<CatalogSummary>("open_folder", { path });
      this.lastError = null;
      await this.loadPage(0);
      return true;
    } catch (error) {
      this.folderError = toMessage(error);
      this.notify();
      return false;
    }
  }

  /** 表示ソート切替 (WP-4)。成功時はsummaryを適用しページを引き直す。 */
  async setSort(sort: CatalogSort): Promise<boolean> {
    try {
      this.summary = await this.ipc.invoke<CatalogSummary>("set_catalog_sort", { sort });
      this.lastError = null;
      await this.loadPage(this.pageOffset);
      return true;
    } catch (error) {
      this.lastError = toMessage(error);
      this.notify();
      return false;
    }
  }

  private applyPage(page: CatalogPage): void {
    this.pageOffset = page.offset;
    this.pageItems = page.items;
    this.pageHasMore = page.has_more;
    this.notify();
  }

  private applyProgress(progress: ScanProgressEvent): void {
    this.progress = progress;
    this.notify();
    if (progress.done) {
      void this.refreshSummary();
    }
  }

  private async applyChanged(event: {
    generation: number;
    reason: string;
  }): Promise<void> {
    if (this.summary === null || event.generation !== this.summary.generation) {
      await this.refreshSummary();
    }
    // 走査中の成長も反映するため、現在オフセットでページを引き直す
    await this.loadPage(this.pageOffset);
  }
}
