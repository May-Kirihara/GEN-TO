/**
 * コレクション (manual / smart) のフロントエンドストア (MVP WP-6)。
 *
 * - command呼び出し・イベント購読を注入 (CollectionsIpc) してテスト容易にする
 * - 一覧・作成・改名・削除・項目追加/削除/並び替え・メモ・スマート条件の
 *   保存/更新/再評価・中央グリッド表示 (collection_show) を担う
 * - `collections:changed` イベントで一覧を自動再取得 (左ペイン追従)
 * - 条件ビルダー (評価≥/選別/お気に入り/タグ/形式。AND結合) の純粋関数
 *   `buildQueryJson` / `parseQueryJson` / `planReorder` はvitest対象
 * - 型契約は docs/dev/api-contract.md §34 (WP-6)。DTOはsnake_case
 */

/** Tauri IPC境界の注入用アダプター (本物は@tauri-apps/api、試験はモック)。 */
export type CollectionsIpc = {
  invoke: <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
  listen: <T>(
    event: string,
    handler: (event: { payload: T }) => void,
  ) => Promise<() => void>;
};

/** コレクションDTO (backend CollectionDto。snake_case)。 */
export type CollectionDto = {
  /** 10進文字列 (コレクション行ID。AssetIdではない)。 */
  id: string;
  uuid: string;
  name: string;
  kind: "manual" | "smart";
  /** smartのみ (正規形JSON)。 */
  query_json?: string;
  /** manual=項目数 / smart=評価件数。 */
  item_count: number;
  created_at: string;
  updated_at: string;
};

/** 手動コレクション項目DTO。 */
export type CollectionItemDto = {
  /** 10進文字列 (stable AssetId)。 */
  asset_id: string;
  position: number;
  note?: string | null;
};

/** `collection_items` の応答。 */
export type CollectionItemsDto = {
  total: number;
  items: CollectionItemDto[];
};

/** `smart_collection_items` の応答。 */
export type SmartItemsDto = {
  total: number;
  asset_ids: string[];
};

/** `collections:changed` イベントペイロード。 */
export type CollectionsChangedEvent = {
  reason: "created" | "renamed" | "deleted" | "items" | "query" | "refreshed";
  collection_id?: string;
};

/** `collection_show` の応答 (get_catalog_summary と同じ形状)。 */
export type CollectionShowSummary = {
  generation: number;
  mode: string;
  total: number;
  scanning: boolean;
  sort: string;
};

// ===== 条件ビルダー (純粋関数) =====

/** 条件型のUI識別子 (backend SmartConditionの "type" と同じ文字列)。 */
export type SmartConditionType =
  | "rating_ge"
  | "cull_state"
  | "favorite"
  | "has_tag"
  | "mime_type";

export const CONDITION_TYPES: ReadonlyArray<{
  type: SmartConditionType;
  label: string;
}> = [
  { type: "rating_ge", label: "評価 ≥" },
  { type: "cull_state", label: "選別状態 =" },
  { type: "favorite", label: "お気に入り =" },
  { type: "has_tag", label: "タグを含む" },
  { type: "mime_type", label: "形式 =" },
];

export const MIME_CHOICES: readonly string[] = [
  "image/png",
  "image/jpeg",
  "image/webp",
  "image/gif",
  "image/bmp",
  "image/tiff",
  "image/avif",
];

/**
 * 条件ビルダーのドラフト (編集途中状態)。値はすべて文字列で保持し
 * buildQueryJsonで検証する (UIは自由入力を許し、確定時にエラー表示)。
 */
export type ConditionDraft = {
  type: SmartConditionType;
  /** rating_ge: "0"〜"5" / cull_state: 値文字列 / favorite: "true"/"false" /
   * has_tag: タグ名 / mime_type: MIME文字列。 */
  value: string;
};

/**
 * ドラフト列からquery_json文字列を構築する (AND結合)。
 * 不正な値はnull (呼び出し側がエラー表示)。出力はbackendの厳密
 * パーサ (SmartQuery::parse) を通る正規形。
 */
export function buildQueryJson(drafts: readonly ConditionDraft[]): string | null {
  const conditions: Array<Record<string, unknown>> = [];
  for (const draft of drafts) {
    switch (draft.type) {
      case "rating_ge": {
        if (!/^[0-5]$/.test(draft.value)) {
          return null;
        }
        conditions.push({ type: "rating_ge", value: Number(draft.value) });
        break;
      }
      case "cull_state": {
        if (
          draft.value !== "unselected" &&
          draft.value !== "picked" &&
          draft.value !== "rejected"
        ) {
          return null;
        }
        conditions.push({ type: "cull_state", value: draft.value });
        break;
      }
      case "favorite": {
        if (draft.value !== "true" && draft.value !== "false") {
          return null;
        }
        conditions.push({ type: "favorite", value: draft.value === "true" });
        break;
      }
      case "has_tag": {
        if (draft.value.trim().length === 0) {
          return null;
        }
        conditions.push({ type: "has_tag", name: draft.value });
        break;
      }
      case "mime_type": {
        if (draft.value.trim().length === 0) {
          return null;
        }
        conditions.push({ type: "mime_type", value: draft.value });
        break;
      }
    }
  }
  return JSON.stringify({ conditions });
}

/** 保存済みquery_jsonをビルダードラフトへ戻す (編集用)。不正ならnull。 */
export function parseQueryJson(json: string | null | undefined): ConditionDraft[] | null {
  if (json === null || json === undefined) {
    return null;
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(json);
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null) {
    return null;
  }
  const conditions = (parsed as Record<string, unknown>)["conditions"];
  if (!Array.isArray(conditions)) {
    return null;
  }
  const drafts: ConditionDraft[] = [];
  for (const entry of conditions) {
    if (typeof entry !== "object" || entry === null) {
      return null;
    }
    const record = entry as Record<string, unknown>;
    const type = record["type"];
    switch (type) {
      case "rating_ge":
      case "cull_state":
      case "mime_type": {
        const value = record["value"];
        if (typeof value !== "string" && typeof value !== "number") {
          return null;
        }
        drafts.push({ type, value: String(value) });
        break;
      }
      case "favorite": {
        drafts.push({
          type,
          value: typeof record["value"] === "boolean" ? String(record["value"]) : "",
        });
        break;
      }
      case "has_tag": {
        const name = record["name"];
        if (typeof name !== "string") {
          return null;
        }
        drafts.push({ type, value: name });
        break;
      }
      default:
        return null;
    }
  }
  return drafts;
}

/** 並び替え操作の種類 (manualのみ。MVPの簡易UI)。 */
export type ReorderTarget =
  | { kind: "front" }
  | { kind: "back" }
  | { kind: "position"; index: number };

/**
 * 現在の全順序と選択から新しい全順序を計算する (純粋関数)。
 * 選択は現在順序内の相対順を保って移動する。位置指定は0-based
 * (選択ブロックの先頭がそこに来る)。不正indexはnull。
 */
export function planReorder(
  currentIds: readonly string[],
  selectedIds: readonly string[],
  target: ReorderTarget,
): string[] | null {
  const selected = new Set(selectedIds);
  const moving: string[] = [];
  const staying: string[] = [];
  for (const id of currentIds) {
    if (selected.has(id)) {
      moving.push(id);
    } else {
      staying.push(id);
    }
  }
  if (moving.length === 0) {
    return null;
  }
  if (target.kind === "front") {
    return [...moving, ...staying];
  }
  if (target.kind === "back") {
    return [...staying, ...moving];
  }
  // position: 残り項目の間へ挿入 (movingを除いた世界での位置)
  const index = target.index;
  if (index < 0 || index > staying.length) {
    return null;
  }
  return [...staying.slice(0, index), ...moving, ...staying.slice(index)];
}

// ===== ストア =====

function toMessage(value: unknown): string {
  return value instanceof Error ? value.message : String(value);
}

/** コレクションUI状態ストア。 */
export class CollectionsStore {
  collections: CollectionDto[] = [];
  lastError: string | null = null;
  version = 0;

  private readonly unlisteners: Array<() => void> = [];
  private readonly changeListeners = new Set<() => void>();
  private readonly changedListeners = new Set<(event: CollectionsChangedEvent) => void>();

  constructor(private readonly ipc: CollectionsIpc) {}

  /** 状態変化 (一覧・エラー) の通知。 */
  onChange(listener: () => void): () => void {
    this.changeListeners.add(listener);
    return () => {
      this.changeListeners.delete(listener);
    };
  }

  /** collections:changed の購読 (Appが表示追従に使用)。 */
  onCollectionsChanged(
    listener: (event: CollectionsChangedEvent) => void,
  ): () => void {
    this.changedListeners.add(listener);
    return () => {
      this.changedListeners.delete(listener);
    };
  }

  private notify(): void {
    this.version++;
    for (const listener of this.changeListeners) {
      listener();
    }
  }

  async init(): Promise<() => void> {
    await this.refresh();
    const unlisten = await this.ipc.listen<CollectionsChangedEvent>(
      "collections:changed",
      (event) => {
        void this.refresh();
        for (const listener of this.changedListeners) {
          listener(event.payload);
        }
      },
    );
    this.unlisteners.push(unlisten);
    return () => this.dispose();
  }

  dispose(): void {
    for (const unlisten of this.unlisteners.splice(0)) {
      unlisten();
    }
  }

  async refresh(): Promise<void> {
    try {
      this.collections = await this.ipc.invoke<CollectionDto[]>("collections_list");
      this.lastError = null;
    } catch (error) {
      this.lastError = toMessage(error);
    }
    this.notify();
  }

  private async call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
    try {
      const result = await this.ipc.invoke<T>(command, args);
      this.lastError = null;
      return result;
    } catch (error) {
      this.lastError = toMessage(error);
      this.notify();
      throw error;
    }
  }

  // ===== 操作 (失敗はthrow。呼び出し側が通知へ出す) =====

  createManual(name: string): Promise<CollectionDto> {
    return this.call<CollectionDto>("collection_create", { name });
  }

  createSmart(name: string, queryJson: string): Promise<CollectionDto> {
    return this.call<CollectionDto>("smart_collection_create", { name, queryJson });
  }

  rename(collectionId: string, name: string): Promise<CollectionDto> {
    return this.call<CollectionDto>("collection_rename", { collectionId, name });
  }

  remove(collectionId: string): Promise<void> {
    return this.call<void>("collection_delete", { collectionId });
  }

  addItems(collectionId: string, ids: readonly string[]): Promise<void> {
    return this.call<void>("collection_add_items", {
      collectionId,
      ids: [...ids],
    });
  }

  removeItem(collectionId: string, assetId: string): Promise<void> {
    return this.call<void>("collection_remove_item", { collectionId, assetId });
  }

  reorder(collectionId: string, assetIds: readonly string[]): Promise<void> {
    return this.call<void>("collection_reorder", {
      collectionId,
      assetIds: [...assetIds],
    });
  }

  items(
    collectionId: string,
    offset = 0,
    limit = 500,
  ): Promise<CollectionItemsDto> {
    return this.call<CollectionItemsDto>("collection_items", {
      collectionId,
      offset,
      limit,
    });
  }

  setItemNote(
    collectionId: string,
    assetId: string,
    note: string | null,
  ): Promise<CollectionItemDto> {
    return this.call<CollectionItemDto>("collection_set_item_note", {
      collectionId,
      assetId,
      note,
    });
  }

  updateSmart(collectionId: string, name: string, queryJson: string): Promise<CollectionDto> {
    return this.call<CollectionDto>("smart_collection_update", {
      collectionId,
      name,
      queryJson,
    });
  }

  smartItems(collectionId: string, offset = 0, limit = 500): Promise<SmartItemsDto> {
    return this.call<SmartItemsDto>("smart_collection_items", {
      collectionId,
      offset,
      limit,
    });
  }

  refreshSmart(collectionId: string): Promise<CollectionDto> {
    return this.call<CollectionDto>("smart_collection_refresh", { collectionId });
  }

  show(collectionId: string): Promise<CollectionShowSummary> {
    return this.call<CollectionShowSummary>("collection_show", { collectionId });
  }
}
