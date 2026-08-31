/**
 * コレクションストア・条件ビルダー・並び替え計画のテスト (MVP WP-6)。
 */
import { describe, expect, it, vi } from "vitest";
import {
  buildQueryJson,
  CollectionsStore,
  parseQueryJson,
  planReorder,
  type CollectionDto,
  type CollectionsIpc,
} from "./collections";

function mockIpc(
  invokeImpl?: (command: string, args?: Record<string, unknown>) => Promise<unknown>,
): {
  ipc: CollectionsIpc;
  invokes: Array<{ command: string; args?: Record<string, unknown> }>;
} {
  const invokes: Array<{ command: string; args?: Record<string, unknown> }> = [];
  const ipc: CollectionsIpc = {
    invoke: <T>(command: string, args?: Record<string, unknown>) => {
      invokes.push({ command, args });
      if (invokeImpl) {
        return invokeImpl(command, args) as Promise<T>;
      }
      return Promise.resolve([] as unknown as T);
    },
    listen: () => Promise.resolve(() => undefined),
  };
  return { ipc, invokes };
}

const dto = (id: string, kind: "manual" | "smart", itemCount = 0): CollectionDto => ({
  id,
  uuid: `00000000-0000-7000-8000-0000000000${id}`,
  name: `コレクション${id}`,
  kind,
  item_count: itemCount,
  created_at: "2026-08-31T00:00:00.000Z",
  updated_at: "2026-08-31T00:00:00.000Z",
});

// ===== buildQueryJson =====

describe("buildQueryJson", () => {
  it("全条件型をAND結合の正規形JSONへ構築する", () => {
    const json = buildQueryJson([
      { type: "rating_ge", value: "4" },
      { type: "cull_state", value: "picked" },
      { type: "favorite", value: "true" },
      { type: "has_tag", value: "Sky" },
      { type: "mime_type", value: "image/png" },
    ]);
    expect(json).toBe(
      '{"conditions":[' +
        '{"type":"rating_ge","value":4},' +
        '{"type":"cull_state","value":"picked"},' +
        '{"type":"favorite","value":true},' +
        '{"type":"has_tag","name":"Sky"},' +
        '{"type":"mime_type","value":"image/png"}]}',
    );
  });

  it("空条件は全件一致 (空配列)", () => {
    expect(buildQueryJson([])).toBe('{"conditions":[]}');
  });

  it("不正値はnull (rating範囲外・未知cull・空タグ)", () => {
    expect(buildQueryJson([{ type: "rating_ge", value: "6" }])).toBeNull();
    expect(buildQueryJson([{ type: "rating_ge", value: "-1" }])).toBeNull();
    expect(buildQueryJson([{ type: "rating_ge", value: "3.5" }])).toBeNull();
    expect(buildQueryJson([{ type: "cull_state", value: "gone" }])).toBeNull();
    expect(buildQueryJson([{ type: "favorite", value: "yes" }])).toBeNull();
    expect(buildQueryJson([{ type: "has_tag", value: "   " }])).toBeNull();
    expect(buildQueryJson([{ type: "mime_type", value: "" }])).toBeNull();
  });
});

describe("parseQueryJson", () => {
  it("保存済みJSONをドラフトへ戻せる (編集ラウンドトリップ)", () => {
    const drafts = parseQueryJson(buildQueryJson([
      { type: "rating_ge", value: "3" },
      { type: "favorite", value: "false" },
      { type: "has_tag", value: "sky" },
    ]));
    expect(drafts).toEqual([
      { type: "rating_ge", value: "3" },
      { type: "favorite", value: "false" },
      { type: "has_tag", value: "sky" },
    ]);
  });

  it("不正JSON・未知条件型はnull", () => {
    expect(parseQueryJson(null)).toBeNull();
    expect(parseQueryJson("not json")).toBeNull();
    expect(parseQueryJson("{}")).toBeNull();
    expect(parseQueryJson('{"conditions":[{"type":"unknown","value":1}]}')).toBeNull();
    expect(parseQueryJson('{"conditions":"x"}')).toBeNull();
  });
});

// ===== planReorder =====

describe("planReorder", () => {
  const current = ["a", "b", "c", "d", "e"];

  it("選択を先頭へ (選択内の相対順を保持)", () => {
    expect(planReorder(current, ["c", "e"], { kind: "front" })).toEqual([
      "c",
      "e",
      "a",
      "b",
      "d",
    ]);
  });

  it("選択を末尾へ", () => {
    expect(planReorder(current, ["a", "b"], { kind: "back" })).toEqual([
      "c",
      "d",
      "e",
      "a",
      "b",
    ]);
  });

  it("位置指定 (残り世界での0-based位置へ挿入)", () => {
    // 選択d/eを除くと [a,b,c]。index=1 → a, [d,e], b, c
    expect(planReorder(current, ["d", "e"], { kind: "position", index: 1 })).toEqual([
      "a",
      "d",
      "e",
      "b",
      "c",
    ]);
    // 境界: 0=先頭と同義 / staying.length=末尾と同義
    expect(planReorder(current, ["b"], { kind: "position", index: 0 })![0]).toBe("b");
    const back = planReorder(current, ["b"], { kind: "position", index: 4 });
    expect(back![4]).toBe("b");
  });

  it("選択なし・不正indexはnull", () => {
    expect(planReorder(current, [], { kind: "front" })).toBeNull();
    expect(planReorder(current, ["a"], { kind: "position", index: -1 })).toBeNull();
    expect(planReorder(current, ["a"], { kind: "position", index: 5 })).toBeNull();
  });

  it("全選択の先頭移動は順序不変", () => {
    expect(planReorder(current, [...current], { kind: "front" })).toEqual(current);
  });
});

// ===== CollectionsStore =====

describe("CollectionsStore", () => {
  it("init/refresh: 一覧を取得して通知する", async () => {
    const { ipc } = mockIpc(<T>(command: string) => {
      if (command === "collections_list") {
        return Promise.resolve([dto("1", "manual", 3)] as unknown as T);
      }
      return Promise.resolve([] as unknown as T);
    });
    const store = new CollectionsStore(ipc);
    const seen: number[] = [];
    const stop = store.onChange(() => seen.push(store.version));
    await store.init();
    expect(store.collections).toEqual([dto("1", "manual", 3)]);
    expect(store.lastError).toBeNull();
    expect(seen.length).toBeGreaterThanOrEqual(1);
    stop();
    store.dispose();
  });

  it("一覧取得失敗はlastErrorへ載せて通知", async () => {
    const { ipc } = mockIpc(<T>(command: string): Promise<T> => {
      if (command === "collections_list") {
        return Promise.reject(new Error("db offline"));
      }
      return Promise.resolve([] as unknown as T);
    });
    const store = new CollectionsStore(ipc);
    await store.refresh();
    expect(store.lastError).toContain("db offline");
    expect(store.collections).toEqual([]);
  });

  it("追加フロー: addItems→items→reorder→show のコマンド引数", async () => {
    const { ipc, invokes } = mockIpc(<T>(command: string): Promise<T> => {
      if (command === "collection_items") {
        return Promise.resolve({
          total: 2,
          items: [
            { asset_id: "7", position: 0, note: null },
            { asset_id: "3", position: 1, note: "メモ" },
          ],
        } as unknown as T);
      }
      if (command === "collection_show") {
        return Promise.resolve({
          generation: 4,
          mode: "library",
          total: 2,
          scanning: false,
          sort: "name",
        } as unknown as T);
      }
      return Promise.resolve(null as unknown as T);
    });
    const store = new CollectionsStore(ipc);
    await store.addItems("5", ["7", "3"]);
    const page = await store.items("5");
    expect(page.total).toBe(2);
    expect(page.items[1].note).toBe("メモ");
    await store.reorder("5", ["3", "7"]);
    await store.setItemNote("5", "7", "追加メモ");
    const summary = await store.show("5");
    expect(summary.total).toBe(2);

    const byCommand = (name: string) => invokes.filter((i) => i.command === name);
    expect(byCommand("collection_add_items")[0].args).toEqual({
      collectionId: "5",
      ids: ["7", "3"],
    });
    expect(byCommand("collection_reorder")[0].args).toEqual({
      collectionId: "5",
      assetIds: ["3", "7"],
    });
    expect(byCommand("collection_set_item_note")[0].args).toEqual({
      collectionId: "5",
      assetId: "7",
      note: "追加メモ",
    });
  });

  it("操作失敗はthrowしてlastErrorへ載せる", async () => {
    const { ipc } = mockIpc(<T>(command: string): Promise<T> => {
      if (command === "collection_create") {
        return Promise.reject(new Error("コレクションの操作を受け付けられません"));
      }
      return Promise.resolve(null as unknown as T);
    });
    const store = new CollectionsStore(ipc);
    await expect(store.createManual("x")).rejects.toThrow("受け付けられません");
    expect(store.lastError).toContain("受け付けられません");
  });

  it("collections:changedイベントで一覧再取得とリスナー呼出が走る", async () => {
    const invoke = vi.fn(
      (command: string): Promise<unknown> => {
        if (command === "collections_list") {
          return Promise.resolve([dto("2", "smart", 9)]);
        }
        return Promise.resolve(null);
      },
    );
    const listeners: Array<(event: { payload: unknown }) => void> = [];
    const ipc: CollectionsIpc = {
      invoke: <T>(command: string) => invoke(command) as Promise<T>,
      listen: (_event, handler) => {
        listeners.push(handler as (event: { payload: unknown }) => void);
        return Promise.resolve(() => undefined);
      },
    };
    const store = new CollectionsStore(ipc);
    const events: string[] = [];
    store.onCollectionsChanged((event) => events.push(event.reason));
    await store.init();
    expect(store.collections).toEqual([dto("2", "smart", 9)]);
    for (const listener of listeners) {
      listener({ payload: { reason: "items", collection_id: "2" } });
    }
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(events).toEqual(["items"]);
    expect(store.collections[0].item_count).toBe(9);
  });
});
