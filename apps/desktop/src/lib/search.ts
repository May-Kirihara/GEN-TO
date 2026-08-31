/**
 * 検索 (WP-8) のフロントエンドストア。
 *
 * - 検索構文はbackend (gento-search) が構文解析する。ここでは
 *   クエリ文字列 + GUIフィルタ (AST term JSON) + スコープを送る
 * - 検索結果はカタログの「search」ソースとして表示される
 *   (catalog:changed → CatalogStore/GridStoreが追従)
 * - 一致根拠 (evidence) は表示中ページ分を要求してassetId→一致列の
 *   写像として保持する (InfoPanel・セルツールチップ用)
 * - 型契約は docs/dev/api-contract.md §42
 */
import type { CatalogSummary } from "./catalog-types";

export type SearchIpc = {
  invoke: <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
};

/** `search_run` の応答。 */
export type SearchRunResult = {
  summary: CatalogSummary;
  total: number;
  shown: number;
  elapsedMs: number;
  warnings: string[];
  queryJson: string;
};

/** `search_evidence` の応答項目。 */
export type SearchFieldMatch = { field: string; snippet: string };
export type SearchEvidenceItem = { assetId: string; matches: SearchFieldMatch[] };

/** GUIフィルタ (未設定項目はnull/空文字)。検索構文とAND合成される。 */
export type GuiFilters = {
  /** 評価の下限 (0..=5)。 */
  ratingMin: number | null;
  /** 選別状態。 */
  cull: "picked" | "rejected" | "unselected" | null;
  /** お気に入り。 */
  favorite: boolean | null;
  /** 形式 (拡張子または完全MIME)。 */
  type: string;
  /** タグ (正規化はbackendが実施)。 */
  tag: string;
  /** 追加日範囲 (YYYY-MM-DD。片端のみ可)。 */
  addedFrom: string;
  addedTo: string;
};

export const EMPTY_FILTERS: GuiFilters = {
  ratingMin: null,
  cull: null,
  favorite: null,
  type: "",
  tag: "",
  addedFrom: "",
  addedTo: "",
};

/** フィルタが1つでも設定されているか。 */
export function filtersActive(filters: GuiFilters): boolean {
  return (
    filters.ratingMin !== null ||
    filters.cull !== null ||
    filters.favorite !== null ||
    filters.type !== "" ||
    filters.tag !== "" ||
    filters.addedFrom !== "" ||
    filters.addedTo !== ""
  );
}

type IsoDateJson = { year: number; month: number; day: number };

function isoDateOf(value: string): IsoDateJson | null {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(value.trim());
  if (match === null) {
    return null;
  }
  return {
    year: Number(match[1]),
    month: Number(match[2]),
    day: Number(match[3]),
  };
}

function fieldTerm(
  key: string,
  op: string,
  value: unknown,
): { type: string; key: string; op: string; value: unknown } {
  return { type: "field", key, op, value };
}

/**
 * GUIフィルタ → AST term JSON列 (gento-searchのAST直列化と同一形状。
 * day不正な日付はエラー扱い (backendでの曖昧解釈を避ける)。
 */
export function filterTerms(filters: GuiFilters): unknown[] {
  const terms: unknown[] = [];
  if (filters.ratingMin !== null) {
    terms.push(fieldTerm("rating", "ge", { number: filters.ratingMin }));
  }
  if (filters.cull !== null) {
    terms.push(fieldTerm("cull", "eq", { text: filters.cull }));
  }
  if (filters.favorite !== null) {
    terms.push(fieldTerm("fav", "eq", { bool: filters.favorite }));
  }
  if (filters.type !== "") {
    terms.push(fieldTerm("type", "eq", { text: filters.type.trim() }));
  }
  if (filters.tag !== "") {
    terms.push(fieldTerm("tag", "eq", { text: filters.tag.trim() }));
  }
  const from = filters.addedFrom.trim() === "" ? null : isoDateOf(filters.addedFrom);
  const to = filters.addedTo.trim() === "" ? null : isoDateOf(filters.addedTo);
  if (filters.addedFrom.trim() !== "" && from === null) {
    throw new Error("追加日 (開始) は YYYY-MM-DD 形式で指定してください");
  }
  if (filters.addedTo.trim() !== "" && to === null) {
    throw new Error("追加日 (終了) は YYYY-MM-DD 形式で指定してください");
  }
  if (from !== null || to !== null) {
    terms.push(fieldTerm("added", "eq", { date_range: { from, to } }));
  }
  return terms;
}

function toMessage(value: unknown): string {
  return value instanceof Error ? value.message : String(value);
}

export class SearchStore {
  running = false;
  lastResult: SearchRunResult | null = null;
  lastError: string | null = null;
  /** 現在の検索構文 (再実行・保存の既定値)。 */
  lastQuery = "";
  lastFilters: GuiFilters = { ...EMPTY_FILTERS };
  /** assetId → 一致根拠列。 */
  evidence: Record<string, SearchFieldMatch[]> = {};

  private readonly changeListeners = new Set<() => void>();

  constructor(private readonly ipc: SearchIpc) {}

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

  /** 検索を実行し、結果をカタログのsearchソースへ表示する。 */
  async run(
    query: string,
    filters: GuiFilters,
    scope: { rootId?: string; collectionId?: string } = {},
  ): Promise<boolean> {
    this.running = true;
    this.lastError = null;
    this.notify();
    let terms: unknown[];
    try {
      terms = filterTerms(filters);
    } catch (error) {
      this.running = false;
      this.lastError = toMessage(error);
      this.notify();
      return false;
    }
    const args: Record<string, unknown> = { query, filters: terms };
    if (scope.rootId !== undefined) {
      args.scopeRootId = scope.rootId;
    }
    if (scope.collectionId !== undefined) {
      args.scopeCollectionId = scope.collectionId;
    }
    try {
      const result = await this.ipc.invoke<SearchRunResult>("search_run", args);
      this.lastResult = result;
      this.lastQuery = query;
      this.lastFilters = { ...filters };
      this.evidence = {};
      return true;
    } catch (error) {
      this.lastError = toMessage(error);
      return false;
    } finally {
      this.running = false;
      this.notify();
    }
  }

  /** 表示中検索を新しいDB状態で再実行する (メタデータ編集後のrefresh用)。 */
  async refresh(): Promise<boolean> {
    this.running = true;
    this.lastError = null;
    this.notify();
    try {
      const result = await this.ipc.invoke<SearchRunResult>("search_refresh", {});
      this.lastResult = result;
      this.evidence = {};
      return true;
    } catch (error) {
      this.lastError = toMessage(error);
      return false;
    } finally {
      this.running = false;
      this.notify();
    }
  }

  /** 一致根拠を取得して写像へ保持 (未取得のIDのみ要求する)。 */
  async evidenceFor(ids: string[]): Promise<void> {
    const wanted = ids.filter((id) => this.evidence[id] === undefined);
    if (wanted.length === 0) {
      return;
    }
    try {
      const items = await this.ipc.invoke<SearchEvidenceItem[]>("search_evidence", {
        assetIds: wanted,
      });
      for (const item of items) {
        this.evidence[item.assetId] = item.matches;
      }
      this.notify();
    } catch {
      // 根拠表示は補助情報 — 失敗は静かに無視 (検索結果自体は無事)
    }
  }

  /** 保存済み検索 (スマートコレクション) として保存する。 */
  async save(name: string, queryJson: string): Promise<boolean> {
    try {
      await this.ipc.invoke("smart_collection_create", {
        name,
        queryJson: "{}",
        searchJson: queryJson,
      });
      this.lastError = null;
      return true;
    } catch (error) {
      this.lastError = toMessage(error);
      this.notify();
      return false;
    }
  }
}
