/**
 * フロントエンド計測 (WP-7 / フェーズ9 §16)。
 *
 * - 起動タイムマーク: app mount → health OK → 初回ページ受信 の3点を
 *   performance.now() 基準で記録し、開発パネルとログへ出す
 * - PerformanceObserver "longtask" (閾値50ms。WebKit未対応環境あり) と
 *   rAFフレーム間隔ベースのロングフレーム計測 (50ms超) を併設する
 *
 * 計測モードと通常モードの分離 (spike §11): 観測自体はどちらでも動くが、
 * 出力は開発パネル/devtestブリッジ経由に限り、本計測のログ出力はない。
 */

export type StartupMarks = {
  /** app mount時刻 (ms、プロセス基準)。未記録はnull。 */
  mountedAt: number | null;
  /** health command初回成功時刻。 */
  healthOkAt: number | null;
  /** カタログ初回ページ受信時刻。 */
  firstPageAt: number | null;
};

function defaultNow(): number {
  return typeof performance !== "undefined" ? performance.now() : Date.now();
}

/** 起動マークとロングタスク/ロングフレームの計測器。 */
export class PerfMetrics {
  readonly marks: StartupMarks = {
    mountedAt: null,
    healthOkAt: null,
    firstPageAt: null,
  };
  longTasks = 0;
  worstLongTaskMs: number | null = null;
  longFrames = 0;
  worstLongFrameMs: number | null = null;
  /** PerformanceObserver longtask が利用可能だったか (WebKitの対応差記録用)。 */
  longTaskObserverSupported: boolean | null = null;

  private readonly now: () => number;
  private observer: PerformanceObserver | null = null;
  private rafHandle: ReturnType<typeof requestAnimationFrame> | null = null;
  private frameMonitorActive = false;
  private lastFrameAt: number | null = null;
  private readonly frameThresholdMs: number;
  private readonly taskThresholdMs: number;

  constructor(opts?: {
    now?: () => number;
    frameThresholdMs?: number;
    taskThresholdMs?: number;
  }) {
    this.now = opts?.now ?? defaultNow;
    this.frameThresholdMs = opts?.frameThresholdMs ?? 50;
    this.taskThresholdMs = opts?.taskThresholdMs ?? 50;
  }

  markMounted(): void {
    if (this.marks.mountedAt === null) {
      this.marks.mountedAt = this.now();
    }
  }

  markHealthOk(): void {
    if (this.marks.healthOkAt === null) {
      this.marks.healthOkAt = this.now();
    }
  }

  markFirstPage(): void {
    if (this.marks.firstPageAt === null) {
      this.marks.firstPageAt = this.now();
    }
  }

  /** 派生値: mount→health/firstページの所要時間 (未計測部分はnull)。 */
  startupDurations(): {
    mountToHealthMs: number | null;
    mountToFirstPageMs: number | null;
  } {
    const { mountedAt, healthOkAt, firstPageAt } = this.marks;
    return {
      mountToHealthMs:
        mountedAt !== null && healthOkAt !== null ? healthOkAt - mountedAt : null,
      mountToFirstPageMs:
        mountedAt !== null && firstPageAt !== null ? firstPageAt - mountedAt : null,
    };
  }

  /** PerformanceObserver longtask の観測を開始する (未対応環境は記録して戻る)。 */
  startLongTaskObserver(): void {
    if (this.observer !== null || typeof PerformanceObserver === "undefined") {
      return;
    }
    const supported = (
      PerformanceObserver as unknown as { supportedEntryTypes?: string[] }
    ).supportedEntryTypes;
    const entryTypes: string[] = Array.isArray(supported) ? supported : [];
    if (!entryTypes.includes("longtask")) {
      this.longTaskObserverSupported = false;
      return;
    }
    try {
      const observer = new PerformanceObserver((list) => {
        for (const entry of list.getEntries()) {
          if (entry.duration >= this.taskThresholdMs) {
            this.noteLongTask(entry.duration);
          }
        }
      });
      observer.observe({ entryTypes: ["longtask"] });
      this.observer = observer;
      this.longTaskObserverSupported = true;
    } catch {
      this.longTaskObserverSupported = false;
    }
  }

  /** テストおよび相互検証用: ロングタスクを1件記録する。 */
  noteLongTask(durationMs: number): void {
    this.longTasks++;
    if (this.worstLongTaskMs === null || durationMs > this.worstLongTaskMs) {
      this.worstLongTaskMs = durationMs;
    }
  }

  /** rAFループでフレーム間隔を監視する (longtask非対応環境の代替指標)。 */
  startFrameMonitor(): void {
    if (this.frameMonitorActive || typeof requestAnimationFrame === "undefined") {
      return;
    }
    this.frameMonitorActive = true;
    this.lastFrameAt = this.now();
    const tick = (): void => {
      if (!this.frameMonitorActive) {
        return;
      }
      const at = this.now();
      const prev = this.lastFrameAt;
      this.lastFrameAt = at;
      if (prev !== null) {
        const delta = at - prev;
        if (delta >= this.frameThresholdMs) {
          this.longFrames++;
          if (this.worstLongFrameMs === null || delta > this.worstLongFrameMs) {
            this.worstLongFrameMs = delta;
          }
        }
      }
      this.rafHandle = requestAnimationFrame(tick);
    };
    this.rafHandle = requestAnimationFrame(tick);
  }

  /** 監視を停止する (テスト後始末)。 */
  stop(): void {
    this.frameMonitorActive = false;
    this.observer?.disconnect();
    this.observer = null;
    if (this.rafHandle !== null && typeof cancelAnimationFrame === "function") {
      cancelAnimationFrame(this.rafHandle);
    }
    this.rafHandle = null;
    this.lastFrameAt = null;
  }

  /** 指標をリセットする (マークは起動計測のため保持する)。 */
  resetCounters(): void {
    this.longTasks = 0;
    this.worstLongTaskMs = null;
    this.longFrames = 0;
    this.worstLongFrameMs = null;
  }
}

/** アプリ全体で共有する既定インスタンス (App.svelteとdevtestブリッジが参照)。 */
export const perf = new PerfMetrics();
