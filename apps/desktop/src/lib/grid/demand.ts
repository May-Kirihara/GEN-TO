/**
 * サムネイル需要 (demand) の純粋ロジック (WP-5 / docs/dev/api-contract.md §12「未生成時」フロー)。
 *
 * - 可視行範囲から未生成IDを抽出し、優先度 (viewport > prefetch > overscan) ごとにバッチ化
 * - ReadyMap: サイズクラス別の生成済み地図。cache_generationで古いイベントを破棄し、
 *   URL構成には内容感応versionを使う (FG-1)
 * - FailureSet: 生成失敗の記録 (再要求防止)
 */
import type { ThumbReadyItem } from "../thumbnail-types";

/** 行範囲 (endは含む)。 */
export type RowRange = { start: number; end: number };

export type DemandPriority = "viewport" | "prefetch" | "overscan";

export type DemandPlan = Array<{ priority: DemandPriority; ids: string[] }>;

/** scrollTop差分からスクロール方向。閾値未満は0 (静止)。 */
export function directionFromDelta(prev: number, next: number, threshold = 8): 1 | -1 | 0 {
  const delta = next - prev;
  if (delta > threshold) {
    return 1;
  }
  if (delta < -threshold) {
    return -1;
  }
  return 0;
}

/** ReadyMapのエントリ: 世代ガード用のcache_generationとURL用のversion。 */
type ReadyEntry = {
  /** 要求時のcache_generation (キャッシュクリア検出用)。 */
  generation: number;
  /** 内容感応URLバージョン (`?v=`。FG-1)。 */
  version: number;
};

/** サイズクラス別の生成済み地図。世代ガード込み。 */
export class ReadyMap {
  private latestGeneration = 1;
  private readonly maps = new Map<string, Map<string, ReadyEntry>>();

  /** キャッシュクリア等で既知世代を進める。古い世代の項目を一掃する。 */
  noteGeneration(generation: number): void {
    if (generation <= this.latestGeneration) {
      return;
    }
    this.latestGeneration = generation;
    for (const map of this.maps.values()) {
      for (const [id, entry] of map) {
        if (entry.generation < generation) {
          map.delete(id);
        }
      }
    }
  }

  /** thumb:ready項目を適用し、受け入れた件数を返す。 */
  applyReady(items: ThumbReadyItem[]): number {
    let accepted = 0;
    for (const item of items) {
      if (item.cache_generation < this.latestGeneration) {
        continue;
      }
      if (item.cache_generation > this.latestGeneration) {
        // クリアを見逃した場合: 新世代を正とする
        this.noteGeneration(item.cache_generation);
      }
      let map = this.maps.get(item.size_class);
      if (!map) {
        map = new Map();
        this.maps.set(item.size_class, map);
      }
      map.set(item.asset_id, {
        generation: item.cache_generation,
        version: item.version,
      });
      accepted++;
    }
    return accepted;
  }

  has(sizeClass: string, id: string): boolean {
    return this.maps.get(sizeClass)?.has(id) ?? false;
  }

  generation(sizeClass: string, id: string): number | null {
    return this.maps.get(sizeClass)?.get(id)?.generation ?? null;
  }

  /** URL構成用の内容感応バージョン (未生成はnull)。 */
  version(sizeClass: string, id: string): number | null {
    return this.maps.get(sizeClass)?.get(id)?.version ?? null;
  }

  size(sizeClass: string): number {
    return this.maps.get(sizeClass)?.size ?? 0;
  }

  /** カタログgeneration変化時の全クリア。 */
  clear(): void {
    this.maps.clear();
  }
}

/** サイズクラス別の失敗記録。 */
export class FailureSet {
  private readonly sets = new Map<string, Set<string>>();

  add(sizeClass: string, ids: string[]): void {
    let set = this.sets.get(sizeClass);
    if (!set) {
      set = new Set();
      this.sets.set(sizeClass, set);
    }
    for (const id of ids) {
      set.add(id);
    }
  }

  has(sizeClass: string, id: string): boolean {
    return this.sets.get(sizeClass)?.has(id) ?? false;
  }

  /** このクラスの失敗ID一覧 (画面外リトラクト判定の素材、WP-7)。 */
  ids(sizeClass: string): string[] {
    const set = this.sets.get(sizeClass);
    return set ? [...set] : [];
  }

  /** 指定IDの失敗記録を解除する (画面外へ出た→再登場時に再試行させる)。 */
  retract(sizeClass: string, ids: Iterable<string>): void {
    const set = this.sets.get(sizeClass);
    if (!set) {
      return;
    }
    for (const id of ids) {
      set.delete(id);
    }
  }

  clear(): void {
    this.sets.clear();
  }
}

function rowIndices(row: number, columns: number, total: number): number[] {
  const start = row * columns;
  const end = Math.min(total - 1, start + columns - 1);
  const indices: number[] = [];
  for (let index = start; index <= end; index++) {
    indices.push(index);
  }
  return indices;
}

export function pendingKey(sizeClass: string, id: string): string {
  return `${sizeClass}:${id}`;
}

/**
 * 需要計画の対象行 (visible ∪ prefetch ∪ overscan) の和集合を行範囲で返す
 * (WP-7: 画面外へ出た未開始要求のキャンセル判定に使う)。
 *
 * planDemandと同じ規約でprefetchは進行方向側 (direction 0は下向き)。
 * クランプは行わない (呼び出し側でtotalRows/0へクランプする)。
 */
export function keepZoneRows(input: {
  visible: RowRange;
  overscanBefore: number;
  overscanAfter: number;
  direction: 1 | -1 | 0;
  prefetchRows: number;
}): RowRange {
  const { visible, overscanBefore, overscanAfter, direction, prefetchRows } = input;
  const before =
    overscanBefore + (direction < 0 ? prefetchRows : 0);
  const after = overscanAfter + (direction >= 0 ? prefetchRows : 0);
  return { start: visible.start - before, end: visible.end + after };
}

export type PlanDemandInput = {
  /** 可視行範囲 (overscanを含まない)。 */
  visible: RowRange;
  /** 可視範囲の前後のoverscan行数。 */
  overscanBefore: number;
  overscanAfter: number;
  direction: 1 | -1 | 0;
  totalRows: number;
  columns: number;
  total: number;
  /** 進行方向の先読み行数。 */
  prefetchRows: number;
  /** 優先度ごとの要求上限。 */
  maxBatch: number;
  sizeClass: string;
  ready: ReadyMap;
  failures: FailureSet;
  pending: ReadonlySet<string>;
  itemAt: (index: number) => { id: string; state: string } | null;
};

/**
 * 需要バッチを優先度順 (viewport, prefetch, overscan) で構築する。
 *
 * prefetchは可視範囲の進行方向側の隣接N行 (方向なしは下向き)。
 * overscan行がprefetchと重なる場合は先に出たprefetch優先。
 */
export function planDemand(input: PlanDemandInput): DemandPlan {
  const {
    visible,
    overscanBefore,
    overscanAfter,
    direction,
    totalRows,
    columns,
    total,
    prefetchRows,
    maxBatch,
    sizeClass,
    ready,
    failures,
    pending,
    itemAt,
  } = input;

  const lastRow = totalRows - 1;

  type RowEntry = { row: number; priority: DemandPriority };
  const rows: RowEntry[] = [];
  const seenRows = new Set<number>();

  const pushRows = (start: number, end: number, priority: DemandPriority) => {
    if (start > end) {
      // 降順 (上向き先読み): 可視側に近い行から
      for (let row = Math.min(start, lastRow); row >= end; row--) {
        if (row < 0) {
          break;
        }
        if (!seenRows.has(row)) {
          seenRows.add(row);
          rows.push({ row, priority });
        }
      }
      return;
    }
    for (let row = Math.max(start, 0); row <= Math.min(end, lastRow); row++) {
      if (!seenRows.has(row)) {
        seenRows.add(row);
        rows.push({ row, priority });
      }
    }
  };

  pushRows(visible.start, Math.min(visible.end, lastRow), "viewport");
  if (prefetchRows > 0) {
    if (direction >= 0) {
      pushRows(visible.end + 1, visible.end + prefetchRows, "prefetch");
    } else {
      pushRows(visible.start - 1, visible.start - prefetchRows, "prefetch");
    }
  }
  if (overscanBefore > 0) {
    pushRows(visible.start - overscanBefore, visible.start - 1, "overscan");
  }
  if (overscanAfter > 0) {
    pushRows(visible.end + 1, visible.end + overscanAfter, "overscan");
  }

  const buckets = new Map<DemandPriority, string[]>();
  const claimed = new Set<string>();

  for (const { row, priority } of rows) {
    const bucket = buckets.get(priority) ?? [];
    if (bucket.length >= maxBatch) {
      continue;
    }
    for (const index of rowIndices(row, columns, total)) {
      if (bucket.length >= maxBatch) {
        break;
      }
      const item = itemAt(index);
      if (item === null || item.state !== "ready") {
        continue;
      }
      if (ready.has(sizeClass, item.id) || failures.has(sizeClass, item.id)) {
        continue;
      }
      if (pending.has(pendingKey(sizeClass, item.id))) {
        continue;
      }
      if (claimed.has(item.id)) {
        continue;
      }
      claimed.add(item.id);
      bucket.push(item.id);
    }
    buckets.set(priority, bucket);
  }

  const order: DemandPriority[] = ["viewport", "prefetch", "overscan"];
  const plan: DemandPlan = [];
  for (const priority of order) {
    const ids = buckets.get(priority);
    if (ids && ids.length > 0) {
      plan.push({ priority, ids });
    }
  }
  return plan;
}
