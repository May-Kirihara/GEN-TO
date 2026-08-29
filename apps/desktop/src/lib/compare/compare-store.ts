/**
 * 2枚・4枚比較のストア (WP-6 / フェーズ7)。
 *
 * - スロット: {assetId, detached(一時解除), own(detached/sync OFF時のtransform),
 *   isBase相当はbaseSlot index, isOriginal(昇格)}
 * - 同期: 共有 {centerX, centerY, zoomFactor} (正規化中心 + fit相対倍率)。
 *   sync ON かつ非detachedスロットは syncTransform で導出、それ以外は own
 * - 旧画像保持: 各スロットでURL差し替え時にretainedUrlを残しonloadで解除
 * - 昇格: アクティブスロットのみoriginalUrlへ (4枚すべてを無条件原寸展開しない)
 * - フレーム時間: 同期パン中のrAF deltaをFrameLogへ (コンポーネントからtick)
 */
import type { ThumbFailedItem, ThumbReadyItem } from "../thumbnail-types";
import { originalUrl, previewUrl } from "../protocol";
import { ReadyMap } from "../grid/demand";
import {
  clampTransform,
  fitScale,
  fitTransform,
  panBy as panTransform,
  zoomAtPoint,
  type ViewTransform,
  type Viewport,
} from "../viewer/transform";
import { FrameLog } from "../viewer/metrics";
import { fillSlots, nextGroup, swapSlots, type SlotItem } from "./slots";
import {
  initialSync,
  panSync,
  syncTransform,
  zoomSyncAt,
  type SyncState,
} from "./sync";

export type CompareIpc = {
  invoke: <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
  listen: <T>(
    event: string,
    handler: (event: { payload: T }) => void,
  ) => Promise<() => void>;
};

/** カタログ項目の供給源 (GridStoreのpagerを想定)。 */
export type CompareItems = {
  total(): number;
  get(index: number): {
    id: string;
    displayName: string;
    width: number | null;
    height: number | null;
    state: string;
  } | null;
  ensureRange(start: number, end: number): Promise<void>;
  findIndexById(id: string): number | null;
};

const PREVIEW_CLASS = "preview2048";

export type CompareSlot = {
  assetId: string | null;
  displayName: string;
  width: number;
  height: number;
  /** 現在表示するURL。 */
  url: string | null;
  /** 旧画像保持レイヤーのURL (onloadまで表示し続ける)。 */
  retainedUrl: string | null;
  retainedWidth: number;
  retainedHeight: number;
  isOriginal: boolean;
  detached: boolean;
  /** sync OFF / detached時のtransform。 */
  own: ViewTransform;
  viewport: Viewport;
  /** URL束縛世代 (onload照合用)。 */
  key: string;
  note: string | null;
};

function emptySlot(seq: number): CompareSlot {
  return {
    assetId: null,
    displayName: "",
    width: 0,
    height: 0,
    url: null,
    retainedUrl: null,
    retainedWidth: 0,
    retainedHeight: 0,
    isOriginal: false,
    detached: false,
    own: { scale: 1, centerX: 0.5, centerY: 0.5, mode: "fit" },
    viewport: { width: 0, height: 0 },
    key: `s${seq}`,
    note: null,
  };
}

function toMessage(value: unknown): string {
  return value instanceof Error ? value.message : String(value);
}

export class CompareStore {
  count: 2 | 4 = 2;
  slots: CompareSlot[] = [];
  active = 0;
  /** 基準スロット (次グループで画像を保持する)。null=基準なし。 */
  baseSlot: number | null = null;
  syncEnabled = true;
  sync: SyncState = initialSync();
  /** 次グループが読み込みを始めるカタログindex。 */
  nextIndex = 0;
  readonly ready = new ReadyMap();
  readonly frames = new FrameLog();
  stats = { requests: 0, groups: 0, swaps: 0 };
  version = 0;
  lastError: string | null = null;

  private pending = new Set<string>();
  private unlisteners: Array<() => void> = [];
  private disposed = false;
  private seq = 0;
  private readonly changeListeners = new Set<() => void>();

  constructor(
    private readonly ipc: CompareIpc,
    private readonly items: CompareItems,
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
  }

  /** 比較を開く。選択の先頭N件、不足分は現在位置から連続補充。 */
  async open(
    count: 2 | 4,
    selectedIds: readonly string[],
    focusIndex: number | null,
  ): Promise<void> {
    this.count = count;
    this.sync = initialSync();
    this.syncEnabled = true;
    this.baseSlot = null;
    this.active = 0;
    const total = this.items.total();
    const start = focusIndex !== null ? Math.min(Math.max(focusIndex, 0), Math.max(total - 1, 0)) : 0;
    await this.items.ensureRange(start, start + count + 1).catch(() => {});
    const result = fillSlots({
      selectedIds,
      count,
      focusIndex,
      total,
      get: (index) => this.items.get(index),
      findIndexById: (id) => this.items.findIndexById(id),
    });
    this.nextIndex = result.nextIndex;
    this.slots = result.slots.map((item) => this.buildSlot(item));
    this.notify();
  }

  private buildSlot(item: SlotItem | null): CompareSlot {
    const slot = emptySlot(++this.seq);
    if (item === null) {
      slot.note = "項目なし";
      return slot;
    }
    this.applyItemToSlot(slot, item);
    return slot;
  }

  private applyItemToSlot(slot: CompareSlot, item: SlotItem): void {
    if (slot.url !== null) {
      slot.retainedUrl = slot.url;
      slot.retainedWidth = slot.width;
      slot.retainedHeight = slot.height;
    }
    slot.assetId = item.assetId;
    slot.displayName = item.displayName;
    slot.width = item.width;
    slot.height = item.height;
    slot.isOriginal = false;
    slot.key = `s${++this.seq}`;
    slot.note = null;
    slot.own = fitTransform(slot.viewport, { width: item.width, height: item.height });
    const readyGen = this.ready.generation(PREVIEW_CLASS, item.assetId);
    slot.url = readyGen !== null ? previewUrl(item.assetId, readyGen) : null;
    if (slot.url === null) {
      this.requestPreview(item.assetId);
    }
  }

  private async requestPreview(id: string): Promise<void> {
    const key = `${PREVIEW_CLASS}:${id}`;
    if (this.pending.has(key)) {
      return;
    }
    this.pending.add(key);
    this.stats.requests++;
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
    for (const event of items) {
      this.pending.delete(`${event.size_class}:${event.asset_id}`);
      for (const slot of this.slots) {
        if (
          !slot.isOriginal &&
          slot.url === null &&
          slot.assetId === event.asset_id &&
          event.size_class === PREVIEW_CLASS
        ) {
          slot.url = previewUrl(event.asset_id, event.cache_generation);
        }
      }
    }
    this.notify();
  }

  private applyFailed(items: ThumbFailedItem[]): void {
    for (const item of items) {
      this.pending.delete(`${item.size_class}:${item.asset_id}`);
    }
    this.notify();
  }

  /** 次グループへ進める (基準スロットは保持)。UIをブロックしない非同期。 */
  async nextGroup(): Promise<void> {
    const total = this.items.total();
    const span = this.count - (this.baseSlot !== null ? 1 : 0);
    await this.items.ensureRange(this.nextIndex, this.nextIndex + span).catch(() => {});
    if (this.disposed) {
      return;
    }
    const result = nextGroup(
      this.slots.map((slot) =>
        slot.assetId === null
          ? null
          : {
              assetId: slot.assetId,
              displayName: slot.displayName,
              width: slot.width,
              height: slot.height,
            },
      ),
      this.nextIndex,
      this.baseSlot,
      total,
      (index) => this.items.get(index),
    );
    this.nextIndex = result.nextIndex;
    for (let i = 0; i < this.slots.length; i++) {
      const item = result.slots[i];
      if (item !== null) {
        this.applyItemToSlot(this.slots[i], item);
      }
    }
    this.stats.groups++;
    this.notify();
  }

  /** 2枚比較の左右入れ替え。 */
  swap(): void {
    if (this.count !== 2) {
      return;
    }
    this.slots = swapSlots(this.slots);
    if (this.baseSlot !== null) {
      this.baseSlot = this.count - 1 - this.baseSlot;
    }
    this.stats.swaps++;
    this.notify();
  }

  /** 2⇔4切替。既存スロットを保持し、追加分はnextIndexから連続補充。 */
  async setCount(count: 2 | 4): Promise<void> {
    if (count === this.count) {
      return;
    }
    if (count < this.slots.length) {
      this.slots = this.slots.slice(0, count);
      if (this.baseSlot !== null && this.baseSlot >= count) {
        this.baseSlot = null;
      }
      if (this.active >= count) {
        this.active = 0;
      }
    } else {
      const total = this.items.total();
      const need = count - this.slots.length;
      await this.items.ensureRange(this.nextIndex, this.nextIndex + need).catch(() => {});
      // 追加分だけ充填する (既存スロットの位置とずらさない)
      const result = fillSlots({
        selectedIds: [],
        count: need,
        focusIndex: this.nextIndex,
        total,
        get: (index) => this.items.get(index),
        findIndexById: () => null,
      });
      for (let i = 0; i < need; i++) {
        this.slots.push(this.buildSlot(result.slots[i]));
      }
      this.nextIndex = result.nextIndex;
    }
    this.count = count;
    this.notify();
  }

  // ===== スロット選択・同期 =====

  setActive(index: number): void {
    if (index >= 0 && index < this.slots.length && index !== this.active) {
      this.active = index;
      this.notify();
    }
  }

  cycleActive(): void {
    if (this.slots.length > 0) {
      this.setActive((this.active + 1) % this.slots.length);
    }
  }

  /** 基準スロット設定 (nullで解除)。 */
  setBase(index: number | null): void {
    if (index !== null && (index < 0 || index >= this.slots.length)) {
      return;
    }
    this.baseSlot = index;
    this.notify();
  }

  /** 基準固定の切替 (アクティブスロットを基準にする/解除)。 */
  toggleBase(): void {
    this.setBase(this.baseSlot === null ? this.active : null);
  }

  toggleSync(): void {
    if (this.syncEnabled) {
      // OFFへ: 各スロットの現在有効transformをownへ取り込む (位置を維持したまま切り替え)
      for (let i = 0; i < this.slots.length; i++) {
        this.slots[i].own = this.effectiveTransform(i);
      }
      this.syncEnabled = false;
    } else {
      // ONへ: アクティブスロットの現在有効値 (syncEnabled=falseでのown) を
      // 新しい共有状態へ採用する。フラグ反転前に読まないと旧共有値が使われる
      const active = this.effectiveTransform(this.active);
      const slot = this.slots[this.active];
      this.syncEnabled = true;
      if (slot && slot.width > 0) {
        const fit = fitScale(slot.viewport, { width: slot.width, height: slot.height });
        this.sync = {
          centerX: active.centerX,
          centerY: active.centerY,
          zoomFactor: fit > 0 ? active.scale / fit : 1,
        };
      }
    }
    this.notify();
  }

  /** スロット単位の一時解除。解除時にownへ現在値を取り込む。 */
  toggleDetached(index: number): void {
    const slot = this.slots[index];
    if (!slot) {
      return;
    }
    if (!slot.detached) {
      slot.own = this.effectiveTransform(index);
    }
    slot.detached = !slot.detached;
    this.notify();
  }

  /** スロットの有効transform (sync ONかつ非detachedは共有から導出)。 */
  effectiveTransform(index: number): ViewTransform {
    const slot = this.slots[index];
    if (!slot || slot.width <= 0) {
      return { scale: 1, centerX: 0.5, centerY: 0.5, mode: "fit" };
    }
    return this.effectiveTransformFor(index, slot.width, slot.height);
  }

  /** 任意寸法 (保持レイヤー等) での有効transform。 */
  effectiveTransformFor(index: number, width: number, height: number): ViewTransform {
    const slot = this.slots[index];
    const dims = { width, height };
    if (!slot || width <= 0) {
      return { scale: 1, centerX: 0.5, centerY: 0.5, mode: "fit" };
    }
    if (this.syncEnabled && !slot.detached) {
      return syncTransform(this.sync, slot.viewport, dims);
    }
    return clampTransform(slot.own, slot.viewport, dims);
  }

  setSlotViewport(index: number, width: number, height: number): void {
    const slot = this.slots[index];
    if (!slot) {
      return;
    }
    const changed = width !== slot.viewport.width || height !== slot.viewport.height;
    slot.viewport = { width, height };
    if (changed) {
      if (slot.own.mode === "fit" && slot.width > 0) {
        slot.own = fitTransform(slot.viewport, { width: slot.width, height: slot.height });
      }
      this.notify();
    }
  }

  /** ドラッグΔ (スロットiのビューポートpx)。sync ONなら共有状態へ。 */
  dragBy(index: number, dx: number, dy: number): void {
    const slot = this.slots[index];
    if (!slot || slot.width <= 0) {
      return;
    }
    const dims = { width: slot.width, height: slot.height };
    if (this.syncEnabled && !slot.detached) {
      this.sync = panSync(this.sync, slot.viewport, dims, dx, dy);
    } else {
      slot.own = panTransform(slot.own, slot.viewport, dims, dx, dy);
    }
    this.notify();
  }

  /** ホイールズーム (スロットiのポインタ位置中心)。 */
  wheelZoom(index: number, pointer: { x: number; y: number }, factor: number): void {
    const slot = this.slots[index];
    if (!slot || slot.width <= 0) {
      return;
    }
    const dims = { width: slot.width, height: slot.height };
    if (this.syncEnabled && !slot.detached) {
      this.sync = zoomSyncAt(this.sync, slot.viewport, dims, pointer, factor);
    } else {
      slot.own = zoomAtPoint(slot.own, slot.viewport, dims, pointer, factor);
    }
    this.notify();
  }

  /** fit (共有zoomFactor=1) / active基準の原寸。 */
  setView(mode: "fit" | "actual"): void {
    const slot = this.slots[this.active];
    if (!slot || slot.width <= 0) {
      return;
    }
    const dims = { width: slot.width, height: slot.height };
    if (mode === "fit") {
      this.sync = initialSync();
      if (!this.syncEnabled) {
        // sync OFFでは共有リセットが画面へ現れないため、各スロットのownもfitへ戻す
        for (const s of this.slots) {
          if (s.width > 0) {
            s.own = fitTransform(s.viewport, { width: s.width, height: s.height });
          }
        }
      }
    } else if (this.syncEnabled && !slot.detached) {
      // アクティブスロットの実寸を共有zoomFactorへ反映 (他スロットは相対倍率で追従)
      const fit = fitTransform(slot.viewport, dims);
      this.sync = { ...this.sync, zoomFactor: 1 / (fit.scale || 1) };
    } else {
      slot.own = { ...slot.own, scale: 1, mode: "actual" };
      slot.own = clampTransform(slot.own, slot.viewport, dims);
    }
    this.notify();
  }

  // ===== 昇格 =====

  /** アクティブスロットのoriginal昇格 / preview復帰。 */
  promote(useOriginal: boolean): void {
    const slot = this.slots[this.active];
    if (!slot || slot.assetId === null || slot.width <= 0) {
      return;
    }
    if (useOriginal === slot.isOriginal) {
      return;
    }
    if (useOriginal && slot.url === null) {
      return;
    }
    const prevUrl = slot.url;
    const prevW = slot.width;
    const prevH = slot.height;
    if (useOriginal) {
      const gen = this.ready.generation(PREVIEW_CLASS, slot.assetId) ?? 1;
      slot.key = `s${++this.seq}`;
      slot.url = originalUrl(slot.assetId, gen);
      slot.isOriginal = true;
    } else {
      const gen = this.ready.generation(PREVIEW_CLASS, slot.assetId);
      slot.key = `s${++this.seq}`;
      slot.isOriginal = false;
      slot.url = gen !== null ? previewUrl(slot.assetId, gen) : null;
      if (slot.url === null) {
        this.requestPreview(slot.assetId);
      }
    }
    if (prevUrl !== null) {
      slot.retainedUrl = prevUrl;
      slot.retainedWidth = prevW;
      slot.retainedHeight = prevH;
    }
    this.notify();
  }

  /** スロットの<img> onload。保持レイヤーを解除する。 */
  noteLoaded(index: number, key: string): void {
    const slot = this.slots[index];
    if (slot && slot.key === key && slot.retainedUrl !== null) {
      slot.retainedUrl = null;
      this.notify();
    }
  }

  /** スロットの<img> onerror。original失敗時はpreviewへ復帰。 */
  noteLoadError(index: number, key: string): void {
    const slot = this.slots[index];
    if (!slot || slot.key !== key || slot.assetId === null || !slot.isOriginal) {
      return;
    }
    slot.isOriginal = false;
    const gen = this.ready.generation(PREVIEW_CLASS, slot.assetId);
    slot.key = `s${++this.seq}`;
    slot.url = gen !== null ? previewUrl(slot.assetId, gen) : null;
    if (slot.url === null) {
      this.requestPreview(slot.assetId);
    }
    this.lastError = "original画像の読み込みに失敗しました (previewへ復帰)";
    this.notify();
  }

  /** フレーム計測 (同期パン中にコンポーネントのrAFから呼ぶ)。 */
  startFrames(now: number): void {
    this.frames.start(now);
  }

  tickFrame(now: number): void {
    this.frames.tick(now);
  }

  stopFrames(): void {
    this.frames.stop();
  }

  resetFrames(): void {
    this.frames.reset();
  }
}
