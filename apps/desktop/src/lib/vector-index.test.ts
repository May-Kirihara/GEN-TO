/**
 * USearch索引 (WP-12) フロントエンド純粋ヘルパーのテスト。
 */
import { describe, expect, it } from "vitest";
import {
  VectorIndexStore,
  indexProgressText,
  semanticSearchAvailable,
  semanticUnavailableReason,
  type VectorIndexStatus,
} from "./vector-index";

function statusOf(partial: Partial<VectorIndexStatus>): VectorIndexStatus {
  return {
    model: "ruri-v3-70m",
    ready: true,
    indexedCount: 100,
    sqliteCount: 100,
    building: false,
    lastError: null,
    ...partial,
  };
}

describe("semanticSearchAvailable — 意味検索の可用性", () => {
  it("埋め込み有効+索引ready+件数>0で可用", () => {
    expect(semanticSearchAvailable(true, statusOf({}))).toBe(true);
  });

  it("埋め込み無効・状態null・索引未ready・0件は不可", () => {
    expect(semanticSearchAvailable(false, statusOf({}))).toBe(false);
    expect(semanticSearchAvailable(true, null)).toBe(false);
    expect(semanticSearchAvailable(true, statusOf({ ready: false }))).toBe(false);
    expect(semanticSearchAvailable(true, statusOf({ indexedCount: 0 }))).toBe(false);
  });
});

describe("semanticUnavailableReason — 不可理由", () => {
  it("状態に応じた理由を返す", () => {
    expect(semanticUnavailableReason(false, statusOf({}))).toContain("無効");
    expect(semanticUnavailableReason(true, null)).toContain("取得できません");
    expect(semanticUnavailableReason(true, statusOf({ building: true }))).toContain("構築中");
    expect(semanticUnavailableReason(true, statusOf({ sqliteCount: 0 }))).toContain(
      "埋め込みがまだありません",
    );
    expect(semanticUnavailableReason(true, statusOf({ ready: false }))).toContain("準備中");
    expect(
      semanticUnavailableReason(true, statusOf({ ready: false, lastError: "boom" })),
    ).toContain("準備中");
  });
});

describe("indexProgressText — 進捗表示", () => {
  it("件数をカンマ区切りで表示", () => {
    expect(indexProgressText({ done: 1234, total: 50000 })).toBe(
      "索引構築中 1,234 / 50,000",
    );
  });
});

describe("VectorIndexStore — invoke/subscribeラッパー", () => {
  it("status/rebuildを呼ぶ", async () => {
    const calls: string[] = [];
    const invoke = async <T,>(cmd: string): Promise<T> => {
      calls.push(cmd);
      return statusOf({}) as T;
    };
    const store = new VectorIndexStore({
      invoke,
      listen: async () => () => undefined,
    });
    const status = await store.status();
    expect(calls).toContain("vector_index_status");
    expect(status.ready).toBe(true);
    await store.rebuild();
    expect(calls).toContain("vector_index_rebuild");
  });

  it("進捗イベントのJSON文字列payloadをparseして渡す (破損は無視)", async () => {
    const handlers = new Map<string, (payload: string) => void>();
    const listen = async <T,>(
      event: string,
      handler: (event: { payload: T }) => void,
    ): Promise<() => void> => {
      handlers.set(event, (payload) => (handler as (event: { payload: string }) => void)({ payload }));
      return () => undefined;
    };
    const store = new VectorIndexStore({
      invoke: async <T,>() => undefined as T,
      listen,
    });
    const seen: string[] = [];
    await store.subscribeProgress((progress) => {
      seen.push(indexProgressText(progress));
    });
    handlers.get("vector-index:progress")?.('{"done":5,"total":10}');
    handlers.get("vector-index:progress")?.("not-json");
    expect(seen).toEqual(["索引構築中 5 / 10"]);
  });
});
