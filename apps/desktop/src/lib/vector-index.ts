/**
 * USearch索引 (WP-12) のフロントエンド側。
 *
 * - vector_index_status / vector_index_rebuild のinvokeラッパー
 * - 進捗・準備完了イベントの購読 (payloadはbackendがJSON文字列で送出)
 * - 純粋ヘルパー (意味検索可用性・進捗表示文言) はvitest対象
 */

export type VectorIndexIpc = {
  invoke: <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;
  listen: <T>(
    event: string,
    handler: (event: { payload: T }) => void,
  ) => Promise<() => void>;
};

/** `vector_index_status` 応答DTO (backend VectorIndexStatusDto)。 */
export type VectorIndexStatus = {
  model: string;
  ready: boolean;
  indexedCount: number;
  sqliteCount: number;
  building: boolean;
  lastError: string | null;
};

/** `vector-index:progress` ペイロード (JSON文字列をparse後)。 */
export type VectorIndexProgress = {
  done: number;
  total: number;
};

/** `vector-index:ready` ペイロード (JSON文字列をparse後)。 */
export type VectorIndexReady = {
  model: string;
  count: number;
};

/** 埋め込み有効 + 索引準備済み → 意味検索が使えるか。 */
export function semanticSearchAvailable(
  embeddingEnabled: boolean,
  index: VectorIndexStatus | null,
): boolean {
  if (!embeddingEnabled || index === null) {
    return false;
  }
  return index.ready && index.indexedCount > 0;
}

/** 意味検索が使えないときの理由 (スライダーdisabledのtooltip用)。 */
export function semanticUnavailableReason(
  embeddingEnabled: boolean,
  index: VectorIndexStatus | null,
): string {
  if (!embeddingEnabled) {
    return "自然言語検索が無効です (AIパネル→自然言語検索タブで有効化)";
  }
  if (index === null) {
    return "ベクトル索引の状態を取得できません";
  }
  if (index.building) {
    return "ベクトル索引を構築中です…";
  }
  if (index.sqliteCount === 0) {
    return "埋め込みがまだありません (埋め込み完了後に自動構築されます)";
  }
  if (!index.ready || index.indexedCount === 0) {
    return "ベクトル索引を準備中です…";
  }
  if (index.lastError !== null) {
    return `索引エラー: ${index.lastError}`;
  }
  return "ベクトル索引が未構築です";
}

/** 索引構築進捗の一行表示。 */
export function indexProgressText(progress: VectorIndexProgress): string {
  return `索引構築中 ${progress.done.toLocaleString()} / ${progress.total.toLocaleString()}`;
}

/** ベクトル索引ストア (薄いinvokeラッパ + イベント購読)。 */
export class VectorIndexStore {
  private ipc: VectorIndexIpc;

  constructor(ipc: VectorIndexIpc) {
    this.ipc = ipc;
  }

  async status(): Promise<VectorIndexStatus> {
    return this.ipc.invoke<VectorIndexStatus>("vector_index_status");
  }

  async rebuild(): Promise<VectorIndexStatus> {
    return this.ipc.invoke<VectorIndexStatus>("vector_index_rebuild");
  }

  /** 構築進捗イベント購読 (payloadはJSON文字列)。 */
  async subscribeProgress(
    handler: (progress: VectorIndexProgress) => void,
  ): Promise<() => void> {
    return this.ipc.listen<string>("vector-index:progress", (event) => {
      try {
        handler(JSON.parse(event.payload) as VectorIndexProgress);
      } catch {
        // 進捗表示は補助情報 — 破損ペイロードは無視
      }
    });
  }

  /** 構築完了イベント購読。 */
  async subscribeReady(handler: (ready: VectorIndexReady) => void): Promise<() => void> {
    return this.ipc.listen<string>("vector-index:ready", (event) => {
      try {
        handler(JSON.parse(event.payload) as VectorIndexReady);
      } catch {
        // 無視
      }
    });
  }
}
