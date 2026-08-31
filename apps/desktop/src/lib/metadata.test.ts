import { describe, expect, it } from "vitest";
import {
  CAPTION_KINDS,
  MetadataStore,
  canEditMetadata,
  patchOfEvent,
  type AssetUpdatedEvent,
  type MetadataIpc,
  type TagDto,
} from "./metadata";

function fakeIpc() {
  const calls: Array<{ command: string; args?: Record<string, unknown> }> = [];
  const listeners = new Map<string, Array<(payload: unknown) => void>>();
  const ipc: MetadataIpc = {
    invoke: async <T>(command: string, args?: Record<string, unknown>): Promise<T> => {
      calls.push({ command, args });
      if (command === "asset_set_rating" && typeof args?.rating === "number" && args.rating > 5) {
        throw new Error("評価は0〜5で指定してください");
      }
      if (command === "tag_suggest") {
        const prefix = String(args?.prefix ?? "");
        const all: TagDto[] = [
          { id: "1", name: "sky", normalized_name: "sky", usage_count: 3, manually_edited: false },
          { id: "2", name: "skyline", normalized_name: "skyline", usage_count: 1, manually_edited: false },
          { id: "3", name: "sunset", normalized_name: "sunset", usage_count: 2, manually_edited: false },
        ];
        return all.filter((t) => t.normalized_name.startsWith(prefix.toLowerCase())) as T;
      }
      return null as T;
    },
    listen: async <T>(
      event: string,
      handler: (e: { payload: T }) => void,
    ): Promise<() => void> => {
      const list = listeners.get(event) ?? [];
      list.push((payload: unknown) => handler({ payload: payload as T }));
      listeners.set(event, list);
      return () => {
        listeners.set(
          event,
          (listeners.get(event) ?? []).filter((h) => h !== handler),
        );
      };
    },
  };
  return {
    ipc,
    calls,
    emit: (event: string, payload: unknown) => {
      for (const handler of listeners.get(event) ?? []) {
        handler(payload);
      }
    },
  };
}

describe("patchOfEvent", () => {
  it("評価・選別・お気に入りの値をパッチへ変換する", () => {
    const event: AssetUpdatedEvent = {
      ids: ["1"],
      changes: ["rating"],
      rating: 4,
    };
    expect(patchOfEvent(event)).toEqual({ rating: 4 });
  });

  it("cull値はグリッド表現 (keep/reject/undecided) へ写像する", () => {
    expect(
      patchOfEvent({ ids: ["1"], changes: ["cull"], triage: "picked" }),
    ).toEqual({ triage: "keep" });
    expect(
      patchOfEvent({ ids: ["1"], changes: ["cull"], triage: "rejected" }),
    ).toEqual({ triage: "reject" });
    expect(
      patchOfEvent({ ids: ["1"], changes: ["cull"], triage: "unselected" }),
    ).toEqual({ triage: "undecided" });
  });

  it("tags/captionのみのイベントは空パッチ (項目DTOに対応フィールド無し)", () => {
    expect(patchOfEvent({ ids: ["1"], changes: ["tags"] })).toEqual({});
    expect(patchOfEvent({ ids: ["1"], changes: ["caption"] })).toEqual({});
  });

  it("複数フィールドの同時変更にも対応する", () => {
    expect(
      patchOfEvent({ ids: ["1"], changes: ["rating", "favorite"], rating: 5, favorite: true }),
    ).toEqual({ rating: 5, favorite: true });
  });
});

describe("canEditMetadata", () => {
  it("library系表示 (ライブラリ・コレクション・検索結果) のAsset確定項目は編集可", () => {
    expect(canEditMetadata(true, { dbState: "asset" })).toBe(true);
  });

  it("WP-8回帰: 検索結果表示はlibrary系 — 直前がtemp走査でも編集可", () => {
    // 検索実行でカタログソースはlibrary系へ遷移する (api-contract §42:
    // 検索結果はmode=Library系・安定AssetId)。temp由来だからといって
    // 編集不可に戻してはならない。
    expect(canEditMetadata(true, { dbState: "asset" })).toBe(true);
  });

  it("一時走査表示 (未登録フォルダ) はdbStateに関係なく編集不可 — 仕様§4.1", () => {
    expect(canEditMetadata(false, { dbState: "asset" })).toBe(false);
    expect(canEditMetadata(false, { dbState: "adhoc" })).toBe(false);
  });

  it("ハッシュ計算待ち (provisional) は編集不可", () => {
    expect(canEditMetadata(true, { dbState: "provisional" })).toBe(false);
  });

  it("項目未選択は編集不可", () => {
    expect(canEditMetadata(true, null)).toBe(false);
  });
});

describe("MetadataStore", () => {
  it("編集操作を対応するコマンドへ引数付きで送る", async () => {
    const { ipc, calls } = fakeIpc();
    const store = new MetadataStore(ipc);
    await store.setRating(["1", "2"], 3);
    await store.setCull(["1"], "rejected");
    await store.setFavorite(["1", "2"], true);
    await store.addTags(["1"], ["Sky", "blue"]);
    await store.removeTag("1", "9");
    await store.setCaption("1", "user_note", "メモ");
    expect(calls.map((c) => c.command)).toEqual([
      "asset_set_rating",
      "asset_set_cull",
      "asset_set_favorite",
      "asset_tags_add",
      "asset_tags_remove",
      "asset_caption_set",
    ]);
    expect(calls[0].args).toEqual({ ids: ["1", "2"], rating: 3 });
    expect(calls[1].args).toEqual({ ids: ["1"], cull: "rejected" });
    expect(calls[3].args).toEqual({ ids: ["1"], names: ["Sky", "blue"] });
    expect(calls[4].args).toEqual({ assetId: "1", tagId: "9" });
    expect(calls[5].args).toEqual({ assetId: "1", kind: "user_note", body: "メモ" });
  });

  it("失敗時はlastErrorへ載せ、成功でクリアする", async () => {
    const { ipc } = fakeIpc();
    const store = new MetadataStore(ipc);
    let notified = 0;
    store.onChange(() => notified++);
    await expect(store.setRating(["1"], 6)).rejects.toThrow("0〜5");
    expect(store.lastError).toContain("0〜5");
    expect(notified).toBe(1);
    await store.setRating(["1"], 5);
    expect(store.lastError).toBeNull();
  });

  it("asset:updatedイベントを購読者へ配送する", async () => {
    const { ipc, emit } = fakeIpc();
    const store = new MetadataStore(ipc);
    const received: AssetUpdatedEvent[] = [];
    store.onAssetUpdated((event) => received.push(event));
    await store.init();
    emit("asset:updated", { ids: ["7"], changes: ["cull"], triage: "picked" });
    expect(received).toEqual([{ ids: ["7"], changes: ["cull"], triage: "picked" }]);
    store.dispose();
  });

  it("タグ候補はbackendの正規化prefix一致をそのまま受け取る", async () => {
    const { ipc } = fakeIpc();
    const store = new MetadataStore(ipc);
    const hits = await store.suggestTags("SKY", 5);
    expect(hits.map((t) => t.normalized_name)).toEqual(["sky", "skyline"]);
    // 候補のmanualフラグは一覧系ではfalse (asset_tags_listのみ意味を持つ)
    expect(hits.every((t) => !t.manually_edited)).toBe(true);
  });

  it("CAPTION_KINDSは編集対象3種", () => {
    expect(CAPTION_KINDS.map((k) => k.kind)).toEqual(["description", "sd_tags", "user_note"]);
  });
});
