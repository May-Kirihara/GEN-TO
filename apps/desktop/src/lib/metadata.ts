/**
 * アセットメタデータ編集のストア (MVP WP-5)。
 *
 * - 評価・選別・お気に入り・タグ・キャプションの編集をIPCへ渡す。
 *   永続化の正本はDB (backend)。書き込み成功は `asset:updated` イベントで
 *   局所反映する (このストアは楽観更新を行わない — 失敗時の巻き戻しが不要)。
 * - adhoc (未登録フォルダ表示) ではbackendが編集を拒否するため、
 *   呼び出し側 (InfoPanel等) は編集可否を `item.dbState === "asset"` で
 *   判定してUIを無効化する。
 * - Undoは本ストアと対になる `lib/undo.ts` のスタックが担う
 *   (before値の捕捉→逆適用)。ファイル操作のUndoはWP-7で統合。
 */

export type MetadataIpc = {
  invoke: <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
  listen: <T>(
    event: string,
    handler: (event: { payload: T }) => void,
  ) => Promise<() => void>;
};

/** `asset:updated` イベントペイロード。 */
export type AssetUpdatedEvent = {
  /** 10進文字列のAssetId列。 */
  ids: string[];
  /** 変更種別 ("rating" | "cull" | "favorite" | "tags" | "caption")。 */
  changes: string[];
  /** 変更後の評価 (rating変更時のみ)。 */
  rating?: number;
  /** 変更後の選別状態 (cull変更時のみ)。 */
  triage?: CullValue;
  /** 変更後のお気に入り (favorite変更時のみ)。 */
  favorite?: boolean;
};

/** タグDTO (backend TagDto。camelCase)。 */
export type TagDto = {
  id: string;
  name: string;
  normalized_name: string;
  usage_count: number;
  manually_edited: boolean;
};

/** キャプションDTO (backend CaptionDto)。 */
export type CaptionDto = {
  kind: "description" | "sd_tags" | "user_note";
  body: string;
  source: string;
  manually_edited: boolean;
  updated_at: string;
};

/** `asset_detail` の応答 (InfoPanel一括取得用)。 */
export type AssetDetailDto = {
  id: string;
  rating: number;
  triage: "unselected" | "picked" | "rejected";
  favorite: boolean;
  tags: TagDto[];
  captions: CaptionDto[];
};

/** 編集可能なキャプション種別 (表示ラベル付き)。 */
export const CAPTION_KINDS: ReadonlyArray<{
  kind: CaptionDto["kind"];
  label: string;
  hint: string;
}> = [
  { kind: "description", label: "説明キャプション", hint: "自然言語検索用" },
  { kind: "sd_tags", label: "SDタグ列", hint: "学習用 (順序保持)" },
  { kind: "user_note", label: "メモ", hint: "自由記述" },
];

export type CullValue = "picked" | "rejected" | "unselected";

function toMessage(value: unknown): string {
  return value instanceof Error ? value.message : String(value);
}

/** invokeにタイムアウトを付ける (IPC応答喪失時にpendingが残るのを防ぐ。
 * WP-2の教訓: grid/pages.ts と同じ規約)。 */
function withTimeout<T>(promise: Promise<T>, ms: number, what: string): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => {
      reject(new Error(`${what} timed out after ${ms}ms`));
    }, ms);
    promise.then(
      (value) => {
        clearTimeout(timer);
        resolve(value);
      },
      (error) => {
        clearTimeout(timer);
        reject(error);
      },
    );
  });
}

/** セル/InfoPanelへ反映するローカルパッチ (asset:updated受信時に適用)。 */
export type LocalItemPatch = {
  rating?: number;
  triage?: "undecided" | "keep" | "reject";
  favorite?: boolean;
};

/** `asset:updated` から項目DTOへ適用するパッチを導く。
 * (tags/captionは項目DTOに対応フィールドが無いため空 — InfoPanelは
 * detail再取得で追従する)。triage値はbackendのcull値をグリッド表現へ写像。 */
export function patchOfEvent(event: AssetUpdatedEvent): LocalItemPatch {
  const patch: LocalItemPatch = {};
  if (typeof event.rating === "number") {
    patch.rating = event.rating;
  }
  if (typeof event.triage === "string") {
    patch.triage =
      event.triage === "picked" ? "keep" : event.triage === "rejected" ? "reject" : "undecided";
  }
  if (typeof event.favorite === "boolean") {
    patch.favorite = event.favorite;
  }
  return patch;
}

/**
 * メタデータ編集可否 (仕様§4.1)。表示がDBアセット裏付けのlibrary系
 * (ライブラリ表示・コレクション・検索結果 — api-contract §42。検索結果は
 * backend側もCatalogMode::Libraryで安定AssetId) で、項目がAsset確定済みの
 * 場合のみ編集可。一時走査 (未登録フォルダ) とハッシュ計算待ち (provisional)
 * は不可。InfoPanelの編集UI・Appの詳細取得ゲートがこの同一規則を共有する。
 */
export function canEditMetadata(
  libraryBacked: boolean,
  item: { dbState: string } | null,
): boolean {
  return item !== null && libraryBacked && item.dbState === "asset";
}

export class MetadataStore {
  lastError: string | null = null;
  version = 0;

  private readonly changeListeners = new Set<() => void>();
  private readonly updateListeners = new Set<(event: AssetUpdatedEvent) => void>();
  private readonly timeoutMs: number;
  private unlisten: (() => void) | null = null;

  constructor(
    private readonly ipc: MetadataIpc,
    opts?: { timeoutMs?: number },
  ) {
    this.timeoutMs = opts?.timeoutMs ?? 8000;
  }

  /** asset:updated の購読。initを呼ぶこと。 */
  onAssetUpdated(listener: (event: AssetUpdatedEvent) => void): () => void {
    this.updateListeners.add(listener);
    return () => {
      this.updateListeners.delete(listener);
    };
  }

  /** 状態変化 (エラー等) の通知。 */
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

  async init(): Promise<() => void> {
    const unlisten = await this.ipc.listen<AssetUpdatedEvent>("asset:updated", (event) => {
      for (const listener of this.updateListeners) {
        listener(event.payload);
      }
    });
    this.unlisten = unlisten;
    return () => this.dispose();
  }

  dispose(): void {
    this.unlisten?.();
    this.unlisten = null;
  }

  private async call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
    try {
      const result = await withTimeout(this.ipc.invoke<T>(command, args), this.timeoutMs, command);
      this.lastError = null;
      return result;
    } catch (error) {
      this.lastError = toMessage(error);
      this.notify();
      throw error;
    }
  }

  // ===== 編集操作 (失敗はthrow。呼び出し側が通知へ出す) =====

  setRating(ids: readonly string[], rating: number): Promise<void> {
    return this.call("asset_set_rating", { ids: [...ids], rating });
  }

  setCull(ids: readonly string[], cull: CullValue): Promise<void> {
    return this.call("asset_set_cull", { ids: [...ids], cull });
  }

  setFavorite(ids: readonly string[], favorite: boolean): Promise<void> {
    return this.call("asset_set_favorite", { ids: [...ids], favorite });
  }

  addTags(ids: readonly string[], names: readonly string[]): Promise<void> {
    return this.call("asset_tags_add", { ids: [...ids], names: [...names] });
  }

  removeTag(assetId: string, tagId: string): Promise<void> {
    return this.call("asset_tags_remove", { assetId, tagId });
  }

  listTags(assetId: string): Promise<TagDto[]> {
    return this.call<TagDto[]>("asset_tags_list", { assetId });
  }

  suggestTags(prefix: string, limit = 20): Promise<TagDto[]> {
    return this.call<TagDto[]>("tag_suggest", { prefix, limit });
  }

  listAllTags(): Promise<TagDto[]> {
    return this.call<TagDto[]>("tag_list");
  }

  deleteTag(tagId: string): Promise<void> {
    return this.call("tag_delete", { tagId });
  }

  getCaption(assetId: string, kind: CaptionDto["kind"]): Promise<CaptionDto | null> {
    return this.call<CaptionDto | null>("asset_caption_get", { assetId, kind });
  }

  setCaption(assetId: string, kind: CaptionDto["kind"], body: string): Promise<CaptionDto> {
    return this.call<CaptionDto>("asset_caption_set", { assetId, kind, body });
  }

  assetDetail(assetId: string): Promise<AssetDetailDto> {
    return this.call<AssetDetailDto>("asset_detail", { assetId });
  }
}
