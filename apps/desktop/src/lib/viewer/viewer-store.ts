/**
 * 単一画像ビューアのストア (WP-6 / フェーズ6)。
 *
 * - 要求世代ID: ナビゲーションごとに単調増加する世代でロード結果を管理し、
 *   古いロード完了で現在表示を上書きしない (§13「連続入力時の古い要求を破棄」)
 * - 旧画像保持: 新画像のonloadまで前の画像 (retainedレイヤー) を表示し続ける
 * - 表示はpreview2048ベース。actual/昇格時のみoriginalUrlへ差し替え
 * - 未キャッシュ時は request_thumbnails(priority=interactive) → thumb:ready でURL更新
 * - 先読み: 進行方向±N件 (既定1、トグル可)。readyならWebViewキャッシュのウォーム、
 *   未readyならprefetch優先度で生成要求
 * - 計測: nav→onload時間 (成功分のみ) を先読みon/off別にp50/p95/p99
 *
 * ReadyMap (WP-5パターン) をpreview2048用に流用する。画像要素は論理寸法
 * (カタログの実寸) で配置するため、preview→original差し替えで幾何が不変。
 */
import type { ImageListItem } from "../catalog-types";
import type { ThumbFailedItem, ThumbReadyItem } from "../thumbnail-types";
import { previewUrl, originalUrl } from "../protocol";
import { ReadyMap } from "../grid/demand";
import {
  fitTransform,
  panBy as panTransform,
  clampTransform,
  withMode,
  zoomAtPoint,
  zoomFactorOf,
  type TransformMode,
  type ViewTransform,
  type Viewport,
} from "./transform";
import { planPrefetch } from "./prefetch";
import { LatencyLog } from "./metrics";

export type ViewerIpc = {
  invoke: <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
  listen: <T>(
    event: string,
    handler: (event: { payload: T }) => void,
  ) => Promise<() => void>;
};

/** カタログ項目の供給源 (GridStoreのpagerを想定)。 */
export type ItemsProvider = {
  total(): number;
  get(index: number): ImageListItem | null;
  ensureRange(start: number, end: number): Promise<void>;
};

const PREVIEW_CLASS = "preview2048";

/** 表示レイヤー1枚分の状態。keyはURL束縛世代 (Svelteの要素key兼onload照合用)。 */
export type ViewerImage = {
  navGen: number;
  key: string;
  assetId: string | null;
  displayName: string;
  width: number;
  height: number;
  url: string | null;
  isOriginal: boolean;
  /** アニメーション画像 (original直表示・preview/昇格経路をskip。WP-3)。 */
  isAnimated: boolean;
  note: string | null;
  /** ページDTO由来の内容感応バージョン (original昇格時のURLに使う。FG-1)。 */
  version: number | null;
};

function placeholderImage(navGen: number): ViewerImage {
  return {
    navGen,
    key: `g${navGen}`,
    assetId: null,
    displayName: "",
    width: 0,
    height: 0,
    url: null,
    isOriginal: false,
    isAnimated: false,
    note: null,
    version: null,
  };
}

function toMessage(value: unknown): string {
  return value instanceof Error ? value.message : String(value);
}

export class ViewerStore {
  displayIndex = 0;
  current: ViewerImage | null = null;
  /** 旧画像保持レイヤー (新画像のonloadまで表示し続ける)。 */
  retained: ViewerImage | null = null;
  retainedTransform: ViewTransform | null = null;
  transform: ViewTransform = { scale: 1, centerX: 0.5, centerY: 0.5, mode: "fit" };
  viewport: Viewport = { width: 0, height: 0 };
  prefetchEnabled = true;
  /** 前/次の先読み件数 (片側あたり)。 */
  prefetchCount = 1;
  /** preview2048のready地図 (WP-5パターン流用)。 */
  readonly ready = new ReadyMap();
  /** 先読みon別・off別の表示準備時間。 */
  readonly prepOn = new LatencyLog();
  readonly prepOff = new LatencyLog();
  stats = { applied: 0, staleDropped: 0, prefetchReady: 0, requests: 0, prefetchRequests: 0 };
  version = 0;
  lastError: string | null = null;

  private navGen = 0;
  /** 直近ナビゲーションの開始情報 (最新世代のみ保持。計測と適用判定に使う)。 */
  private navStart: { gen: number; ts: number; prefetched: boolean } | null = null;
  /** actual表示に伴う自動昇格かどうか (fit復帰時にpreviewへ戻す対象)。 */
  private autoOriginal = false;
  private lastDirection: 1 | -1 | 0 = 0;
  private pending = new Set<string>();
  private unlisteners: Array<() => void> = [];
  private disposed = false;
  private readonly changeListeners = new Set<() => void>();

  constructor(
    private readonly ipc: ViewerIpc,
    private readonly items: ItemsProvider,
    private readonly now: () => number = () => performance.now(),
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

  /** thumb:ready / thumb:failed の購読。dispose関数を返す。 */
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

  /** ビューアを開く (項目解決・URL決定まで待機する)。 */
  async open(index: number): Promise<void> {
    await this.items.ensureRange(index, index).catch(() => {});
    if (this.disposed) {
      return;
    }
    await this.showInternal(index);
  }

  /** 表示対象の総数 (UIUX-3: 送りボタンの端disabled判定用)。navと同じclamp。 */
  total(): number {
    return this.items.total();
  }

  /** 前/次へ移動。表示indexは即時に切替え、ロードは世代IDで管理する。 */
  nav(delta: number): void {
    if (delta === 0) {
      return;
    }
    const total = this.items.total();
    const next = Math.min(total - 1, Math.max(0, this.displayIndex + delta));
    if (next === this.displayIndex && this.current !== null) {
      return;
    }
    if (delta > 0) {
      this.lastDirection = 1;
    } else {
      this.lastDirection = -1;
    }
    void this.showInternal(next);
  }

  /** 表示切替え (同期部分) と項目解決 (非同期) 。resolveのpromiseを返す。 */
  private showInternal(index: number): Promise<void> {
    const gen = ++this.navGen;
    this.displayIndex = index;
    this.navStart = { gen, ts: this.now(), prefetched: this.prefetchEnabled };
    // 旧画像保持: 表示中の画像があるときだけ保持レイヤーへ回す
    // (未ロードの現行画像で、すでに表示中の保持レイヤーを置き換えない)
    if (this.current !== null && this.current.url !== null) {
      this.retained = this.current;
      this.retainedTransform = this.transform;
    }
    this.current = placeholderImage(gen);
    this.autoOriginal = false;
    this.transform = { scale: 1, centerX: 0.5, centerY: 0.5, mode: "fit" };
    this.notify();
    return this.resolve(index, gen);
  }

  private async resolve(index: number, gen: number): Promise<void> {
    await this.items.ensureRange(index, index).catch(() => {});
    if (this.disposed || gen !== this.navGen) {
      return;
    }
    const item = this.items.get(index);
    const cur = this.current;
    if (cur === null || cur.navGen !== gen) {
      return;
    }
    if (
      item === null ||
      item.state !== "ready" ||
      item.width === null ||
      item.height === null
    ) {
      cur.note = item === null ? "項目を取得できませんでした" : `表示不可 (state: ${item.state})`;
      this.notify();
      return;
    }
    cur.assetId = item.id;
    cur.displayName = item.displayName;
    cur.width = item.width;
    cur.height = item.height;
    cur.version = item.version;
    cur.isAnimated = item.isAnimated;
    this.transform = fitTransform(this.viewport, { width: item.width, height: item.height });
    if (item.isAnimated) {
      // アニメーション資産はpreviewを経由せずoriginalを直接表示する
      // (WebKitの<img>ネイティブ再生に委ねる)。昇格ロジックもskip。
      cur.url = originalUrl(item.id, item.version ?? 1);
      cur.isOriginal = true;
      this.notify();
      this.runPrefetch(index);
      return;
    }
    const readyVersion = this.ready.version(PREVIEW_CLASS, item.id);
    if (readyVersion !== null) {
      cur.url = previewUrl(item.id, readyVersion);
    } else {
      this.requestPreview(item.id, "interactive");
    }
    this.notify();
    this.runPrefetch(index);
  }

  private async requestPreview(id: string, priority: "interactive" | "prefetch"): Promise<void> {
    const key = `${PREVIEW_CLASS}:${id}`;
    if (this.pending.has(key)) {
      return;
    }
    this.pending.add(key);
    if (priority === "interactive") {
      this.stats.requests++;
    } else {
      this.stats.prefetchRequests++;
    }
    try {
      await this.ipc.invoke("request_thumbnails", {
        ids: [id],
        sizeClass: PREVIEW_CLASS,
        priority,
      });
    } catch (error) {
      this.pending.delete(key);
      this.lastError = toMessage(error);
      this.notify();
    }
  }

  private runPrefetch(index: number): void {
    if (!this.prefetchEnabled || this.prefetchCount <= 0) {
      return;
    }
    const total = this.items.total();
    const plan = planPrefetch(index, total, this.prefetchCount, this.lastDirection);
    if (plan.length === 0) {
      return;
    }
    const gen = this.navGen;
    void (async () => {
      const first = plan[0].index;
      const last = plan[plan.length - 1].index;
      await this.items
        .ensureRange(Math.min(first, last, index), Math.max(first, last, index))
        .catch(() => {});
      if (this.disposed || gen !== this.navGen) {
        return;
      }
      for (const entry of plan) {
        const item = this.items.get(entry.index);
        if (
          item === null ||
          item.state !== "ready" ||
          item.width === null ||
          item.height === null ||
          item.isAnimated
        ) {
          // アニメーション資産はpreviewの先読み対象外 (表示に使わないため。
          // originalの先読みウォームは大容量化するため行わない)
          continue;
        }
        const readyVersion = this.ready.version(PREVIEW_CLASS, item.id);
        if (readyVersion !== null) {
          this.stats.prefetchReady++;
          this.warmCache(previewUrl(item.id, readyVersion));
        } else {
          void this.requestPreview(item.id, "prefetch");
        }
      }
    })();
  }

  /** WebViewのHTTPキャッシュをウォームする (URLは表示と同一のため即時ヒットする)。 */
  private warmCache(url: string): void {
    if (typeof Image === "undefined") {
      return;
    }
    const img = new Image();
    img.src = url;
  }

  private applyReady(items: ThumbReadyItem[]): void {
    const accepted = this.ready.applyReady(items);
    if (accepted === 0) {
      return;
    }
    const cur = this.current;
    for (const item of items) {
      this.pending.delete(`${item.size_class}:${item.asset_id}`);
      if (
        cur !== null &&
        !cur.isOriginal &&
        cur.url === null &&
        cur.assetId === item.asset_id &&
        item.size_class === PREVIEW_CLASS
      ) {
        cur.url = previewUrl(item.asset_id, item.version);
      }
    }
    // actual要求中にpreviewが到着した場合: previewを保持レイヤーへ回して
    // originalへ段階昇格する (url未設定だったために昇格を見送った分の回復)
    if (
      cur !== null &&
      cur.url !== null &&
      !cur.isOriginal &&
      cur.assetId !== null &&
      this.transform.mode === "actual"
    ) {
      this.setOriginal(true);
      this.autoOriginal = this.current?.isOriginal ?? false;
    }
    this.notify();
  }

  private applyFailed(items: ThumbFailedItem[]): void {
    for (const item of items) {
      this.pending.delete(`${item.size_class}:${item.asset_id}`);
    }
    this.notify();
  }

  /**
   * <img>のonload通知。keyはレイヤー (ViewerImage.key) 単位。
   * 現行レイヤーの場合のみ計測・保持レイヤー解除を行う。
   */
  noteLoaded(key: string): void {
    const cur = this.current;
    if (cur !== null && cur.key === key) {
      if (this.navStart !== null && this.navStart.gen === cur.navGen) {
        const elapsed = this.now() - this.navStart.ts;
        (this.navStart.prefetched ? this.prepOn : this.prepOff).add(elapsed);
        this.navStart = null;
        this.stats.applied++;
      }
      this.retained = null;
      this.retainedTransform = null;
      this.notify();
      return;
    }
    if (this.retained !== null && this.retained.key === key) {
      // 保持レイヤーの遅延ロード — 状態変更なし
      return;
    }
    this.stats.staleDropped++;
  }

  /** <img>のonerror通知。originalの失敗時はpreviewへ復帰する。 */
  noteLoadError(key: string): void {
    const cur = this.current;
    if (cur === null || cur.key !== key || cur.assetId === null) {
      return;
    }
    if (cur.isAnimated) {
      // アニメーションoriginalの失敗は静的previewへ落とさない (再生が失われる)
      cur.url = null;
      cur.note = "アニメーション画像の読み込みに失敗しました";
      this.lastError = cur.note;
      this.notify();
      return;
    }
    if (cur.isOriginal) {
      cur.isOriginal = false;
      const readyVersion = this.ready.version(PREVIEW_CLASS, cur.assetId);
      cur.url = readyVersion !== null ? previewUrl(cur.assetId, readyVersion) : null;
      if (cur.url === null) {
        this.requestPreview(cur.assetId, "interactive");
      }
      this.lastError = "original画像の読み込みに失敗しました (previewへ復帰)";
      this.notify();
    }
  }

  // ===== transform操作 =====

  setViewport(width: number, height: number): void {
    const changed = width !== this.viewport.width || height !== this.viewport.height;
    this.viewport = { width, height };
    if (changed && this.current !== null && this.current.width > 0) {
      const dims = { width: this.current.width, height: this.current.height };
      if (this.transform.mode === "fit") {
        this.transform = fitTransform(this.viewport, dims);
      } else {
        this.transform = clampTransform(this.transform, this.viewport, dims);
      }
      this.notify();
    }
  }

  private currentDims(): { width: number; height: number } | null {
    if (this.current === null || this.current.width <= 0) {
      return null;
    }
    return { width: this.current.width, height: this.current.height };
  }

  zoomAt(pointer: { x: number; y: number }, factor: number): void {
    const dims = this.currentDims();
    if (dims === null) {
      return;
    }
    this.transform = zoomAtPoint(this.transform, this.viewport, dims, pointer, factor);
    this.notify();
  }

  panBy(dx: number, dy: number): void {
    const dims = this.currentDims();
    if (dims === null) {
      return;
    }
    this.transform = panTransform(this.transform, this.viewport, dims, dx, dy);
    this.notify();
  }

  /** モード切替。actual への切替えでoriginalへ昇格、離れる時にpreviewへ戻す。 */
  setMode(mode: TransformMode): void {
    if (mode === "actual") {
      this.setOriginal(true);
      this.autoOriginal = this.current?.isOriginal ?? false;
    } else if (this.transform.mode === "actual") {
      this.setOriginal(false);
      this.autoOriginal = false;
    } else if (mode === "fit" && this.autoOriginal) {
      // actual→custom→fit 経由でもactual自動昇格分は復帰する (契約: originalはactual/明示昇格時のみ)
      this.setOriginal(false);
      this.autoOriginal = false;
    }
    const dims = this.currentDims();
    if (dims !== null) {
      this.transform = withMode(this.transform, mode, this.viewport, dims);
    } else if (mode === "fit") {
      this.transform = { scale: 1, centerX: 0.5, centerY: 0.5, mode: "fit" };
    }
    this.notify();
  }

  /** ダブルクリック等のfit⇔actual切替。 */
  toggleFitActual(): void {
    this.setMode(this.transform.mode === "fit" ? "actual" : "fit");
  }

  /** 明示的なoriginal昇格 / preview復帰 (actual表示以外でも切り替え可能)。 */
  setOriginal(useOriginal: boolean): void {
    const cur = this.current;
    if (cur === null || cur.assetId === null || cur.width <= 0) {
      return;
    }
    if (cur.isAnimated) {
      return; // アニメーションは常にoriginal直接表示 (昇格ロジック対象外)
    }
    if (useOriginal === cur.isOriginal) {
      return;
    }
    // 明示操作は自動復帰の対象外とする (actual自動昇格フラグの解除)
    this.autoOriginal = false;
    if (useOriginal && cur.url === null) {
      return; // 表示するpreviewがまだ無い
    }
    if (useOriginal) {
      // previewを保持レイヤーへ回してからoriginalへ差し替える (空白を出さない)
      this.retained = { ...cur };
      this.retainedTransform = this.transform;
      // original URLのバージョンはページDTO由来のものを優先 (表示中項目の
      // 現在の内容に対応)。未取得時のみready地図の値へフォールバックする。
      const version =
        cur.version ?? this.ready.version(PREVIEW_CLASS, cur.assetId) ?? 1;
      cur.key = `${cur.key}-o`;
      cur.url = originalUrl(cur.assetId, version);
      cur.isOriginal = true;
    } else {
      const readyVersion = this.ready.version(PREVIEW_CLASS, cur.assetId);
      if (readyVersion !== null) {
        this.retained = { ...cur };
        this.retainedTransform = this.transform;
      }
      cur.key = `${cur.key}-p`;
      cur.isOriginal = false;
      cur.url = readyVersion !== null ? previewUrl(cur.assetId, readyVersion) : null;
      if (cur.url === null) {
        this.requestPreview(cur.assetId, "interactive");
      }
    }
    this.notify();
  }

  zoomFactor(): number {
    const dims = this.currentDims();
    if (dims === null) {
      return 1;
    }
    return zoomFactorOf(this.transform, this.viewport, dims);
  }

  /** 進行中の要求世代ID (連続ナビで古いロードを破棄する管理番号。開発パネル表示用)。 */
  get generation(): number {
    return this.navGen;
  }

  togglePrefetch(): boolean {
    this.prefetchEnabled = !this.prefetchEnabled;
    this.notify();
    return this.prefetchEnabled;
  }

  resetMetrics(): void {
    this.prepOn.reset();
    this.prepOff.reset();
    this.stats = {
      applied: 0,
      staleDropped: 0,
      prefetchReady: 0,
      requests: 0,
      prefetchRequests: 0,
    };
    this.notify();
  }
}
