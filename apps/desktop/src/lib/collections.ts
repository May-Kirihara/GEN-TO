/**
 * コレクション (manual / smart) のフロントエンドストア (MVP WP-6)。
 *
 * - command呼び出し・イベント購読を注入 (CollectionsIpc) してテスト容易にする
 * - 一覧・作成・改名・削除・項目追加/削除/並び替え・メモ・スマート条件の
 *   保存/更新/再評価・中央グリッド表示 (collection_show) を担う
 * - `collections:changed` イベントで一覧を自動再取得 (左ペイン追従)
 * - 条件ビルダー (評価≥/選別/お気に入り/タグ/形式。AND結合) の純粋関数
 *   `buildQueryJson` (v2 AST保存形式で書き込む) / `parseQueryJson` (v2+
 *   旧形式の読み取り互換) / `describeQueryJson` (条件概要表示) /
 *   `planReorder` はvitest対象
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

/** AST field term JSON (gento-search SearchTerm直列化と同一形状。api-contract §42)。 */
function fieldTerm(
  key: string,
  op: string,
  value: unknown,
): { type: string; key: string; op: string; value: unknown } {
  return { type: "field", key, op, value };
}

/**
 * ドラフト列からquery_json文字列を構築する (AND結合)。
 * 出力は保存形式 version 2 (AST正本 — api-contract §42)。
 * QA-WP8よりビルダーの新規書き込みはv2とし、旧 `{"conditions":[...]}`
 * 形式の書き込みは行わない (旧形式は読み取り互換のみ)。
 * 不正な値はnull (呼び出し側がエラー表示)。
 */
export function buildQueryJson(drafts: readonly ConditionDraft[]): string | null {
  const terms: Array<{ type: string; key: string; op: string; value: unknown }> = [];
  for (const draft of drafts) {
    switch (draft.type) {
      case "rating_ge": {
        if (!/^[0-5]$/.test(draft.value)) {
          return null;
        }
        terms.push(fieldTerm("rating", "ge", { number: Number(draft.value) }));
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
        terms.push(fieldTerm("cull", "eq", { text: draft.value }));
        break;
      }
      case "favorite": {
        if (draft.value !== "true" && draft.value !== "false") {
          return null;
        }
        terms.push(fieldTerm("fav", "eq", { bool: draft.value === "true" }));
        break;
      }
      case "has_tag": {
        if (draft.value.trim().length === 0) {
          return null;
        }
        terms.push(fieldTerm("tag", "eq", { text: draft.value }));
        break;
      }
      case "mime_type": {
        if (draft.value.trim().length === 0) {
          return null;
        }
        terms.push(fieldTerm("type", "eq", { text: draft.value }));
        break;
      }
    }
  }
  return JSON.stringify({ version: 2, query: { terms } });
}

/** 保存済みquery_jsonの構造だけを解析したもの。 */
type StoredQueryShape =
  | { kind: "v2"; terms: unknown[] }
  | { kind: "legacy"; conditions: unknown[] };

/** version 2 (AST) と旧形式 (WP-6 conditions) を判別して解析。 */
function parseStoredShape(json: string | null | undefined): StoredQueryShape | null {
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
  const record = parsed as Record<string, unknown>;
  if (record["version"] === 2) {
    const query = record["query"];
    if (typeof query !== "object" || query === null) {
      return null;
    }
    const terms = (query as Record<string, unknown>)["terms"];
    if (!Array.isArray(terms)) {
      return null;
    }
    return { kind: "v2", terms };
  }
  const conditions = record["conditions"];
  if (Array.isArray(conditions)) {
    return { kind: "legacy", conditions };
  }
  return null;
}

/** v2のAST field termが条件ビルダーで表現できる形ならドラフトへ戻す。 */
function draftOfTerm(term: unknown): ConditionDraft | null {
  if (typeof term !== "object" || term === null) {
    return null;
  }
  const record = term as Record<string, unknown>;
  if (record["type"] !== "field") {
    return null;
  }
  const value = record["value"];
  if (typeof value !== "object" || value === null) {
    return null;
  }
  const valueRecord = value as Record<string, unknown>;
  const key = record["key"];
  const op = record["op"];
  if (key === "rating" && op === "ge" && typeof valueRecord["number"] === "number") {
    return { type: "rating_ge", value: String(valueRecord["number"]) };
  }
  if (key === "cull" && op === "eq" && typeof valueRecord["text"] === "string") {
    return { type: "cull_state", value: valueRecord["text"] };
  }
  if (key === "fav" && op === "eq" && typeof valueRecord["bool"] === "boolean") {
    return { type: "favorite", value: String(valueRecord["bool"]) };
  }
  if (key === "tag" && op === "eq" && typeof valueRecord["text"] === "string") {
    return { type: "has_tag", value: valueRecord["text"] };
  }
  if (key === "type" && op === "eq" && typeof valueRecord["text"] === "string") {
    return { type: "mime_type", value: valueRecord["text"] };
  }
  return null;
}

/** 旧形式のconditions列をドラフトへ戻す (読み取り互換)。不正要素はnull。 */
function draftsOfLegacyConditions(conditions: unknown[]): ConditionDraft[] | null {
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

/**
 * 保存済みquery_jsonをビルダードラフトへ戻す (編集用)。
 * version 2 (ビルダー表現可能なfield termsのみ) と旧形式を受け付ける。
 * テキストterm等ビルダーで表現できないものが混ざる場合はnull。
 */
export function parseQueryJson(json: string | null | undefined): ConditionDraft[] | null {
  const shape = parseStoredShape(json);
  if (shape === null) {
    return null;
  }
  if (shape.kind === "legacy") {
    return draftsOfLegacyConditions(shape.conditions);
  }
  const drafts: ConditionDraft[] = [];
  for (const term of shape.terms) {
    const draft = draftOfTerm(term);
    if (draft === null) {
      return null;
    }
    drafts.push(draft);
  }
  return drafts;
}

/** ドラフト1条件の表示ラベル (条件ビルダー由来の概要)。 */
function draftLabel(draft: ConditionDraft): string {
  switch (draft.type) {
    case "rating_ge":
      return `★≥${draft.value}`;
    case "cull_state":
      return draft.value === "picked" ? "採用" : draft.value === "rejected" ? "除外" : "未選別";
    case "favorite":
      return draft.value === "true" ? "お気に入り" : "非お気に入り";
    case "has_tag":
      return `タグ:${draft.value}`;
    case "mime_type":
      return draft.value.replace("image/", "");
  }
}

const OP_SYMBOL: Readonly<Record<string, string>> = {
  eq: "=",
  ne: "≠",
  ge: ">=",
  le: "<=",
  gt: ">",
  lt: "<",
};

/** v2 AST termの値 ({text:..}/{number:..}/{bool:..}/{date_range:..}) 表現。 */
function termValueLabel(key: string, op: string, value: unknown): string | null {
  const symbol = OP_SYMBOL[op] ?? op;
  if (typeof value !== "object" || value === null) {
    return null;
  }
  const record = value as Record<string, unknown>;
  if (record["text"] !== undefined) {
    if (typeof record["text"] !== "string") {
      return null;
    }
    const text = record["text"];
    if (op === "eq") {
      switch (key) {
        case "cull":
          return text === "picked"
            ? "採用"
            : text === "rejected"
              ? "除外"
              : text === "unselected"
                ? "未選別"
                : `選別=${text}`;
        case "tag":
          return `タグ:${text}`;
        case "type":
          return text.replace("image/", "");
        case "path":
          return `パス:${text}`;
        case "name":
          return `名前:${text}`;
        case "note":
          return `メモ:${text}`;
      }
    }
    return `${key}${symbol}${text}`;
  }
  if (record["number"] !== undefined) {
    if (typeof record["number"] !== "number") {
      return null;
    }
    return key === "rating" ? `★${symbol}${record["number"]}` : `${key}${symbol}${record["number"]}`;
  }
  if (record["bool"] !== undefined) {
    if (typeof record["bool"] !== "boolean") {
      return null;
    }
    if (key === "fav" && op === "eq") {
      return record["bool"] ? "お気に入り" : "非お気に入り";
    }
    return `${key}${symbol}${record["bool"]}`;
  }
  const range = record["date_range"];
  if (range !== undefined) {
    if (typeof range !== "object" || range === null) {
      return null;
    }
    const rangeRecord = range as Record<string, unknown>;
    const dateText = (value: unknown): string | null => {
      if (value === null || value === undefined) {
        return "";
      }
      if (typeof value !== "object") {
        return null;
      }
      const date = value as Record<string, unknown>;
      const { year, month, day } = date;
      if (
        typeof year !== "number" ||
        typeof month !== "number" ||
        typeof day !== "number"
      ) {
        return null;
      }
      const pad = (n: number) => String(n).padStart(2, "0");
      return `${year}-${pad(month)}-${pad(day)}`;
    };
    const from = dateText(rangeRecord["from"]);
    const to = dateText(rangeRecord["to"]);
    if (from === null || to === null) {
      return null;
    }
    return `追加日:${from}〜${to}`;
  }
  return null;
}

/** v2 AST term 1件の表示ラベル。構造不明はnull。 */
function termLabel(term: unknown): string | null {
  if (typeof term !== "object" || term === null) {
    return null;
  }
  const record = term as Record<string, unknown>;
  if (record["type"] === "text") {
    return typeof record["value"] === "string" ? `「${record["value"]}」` : null;
  }
  if (record["type"] !== "field") {
    return null;
  }
  const op = typeof record["op"] === "string" ? record["op"] : "";
  return termValueLabel(String(record["key"]), op, record["value"]);
}

/**
 * 保存済みquery_json (version 2 / 旧形式) の条件概要 — 左ペイン一覧の
 * 表示用 (QA-WP8。検索バーから保存したv2検索も「(条件表示不可)」に
 * ならず概要表示する)。未解析のみnull。空条件は「すべて」。
 */
export function describeQueryJson(json: string | null | undefined): string | null {
  const drafts = parseQueryJson(json);
  if (drafts !== null) {
    return drafts.length === 0 ? "すべて" : drafts.map(draftLabel).join(" かつ ");
  }
  const shape = parseStoredShape(json);
  if (shape === null || shape.kind !== "v2") {
    return null;
  }
  const labels: string[] = [];
  for (const term of shape.terms) {
    const label = termLabel(term);
    if (label === null) {
      return null;
    }
    labels.push(label);
  }
  return labels.length === 0 ? "すべて" : labels.join(" かつ ");
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
    // QA-WP8: 新規スマートコレクションはv2 (AST保存形式) で書き込む。
    // searchJsonが正本 (旧queryJson引数は互換用のダミー — backend仕様)。
    return this.call<CollectionDto>("smart_collection_create", {
      name,
      queryJson: "{}",
      searchJson: queryJson,
    });
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
