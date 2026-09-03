/**
 * サムネイル系IPCのDTO・イベント型 (docs/dev/api-contract.md §7以降と一对一)。
 *
 * summary系・イベントpayloadはsnake_case (WP-2の規約に同じ)。
 */

/** get_thumbnail_stats の戻り値。 */
export type ThumbnailStats = {
  generated: number;
  hits: number;
  misses: number;
  /** hits / (hits + misses)。データなし (0/0) の場合は null。 */
  hit_rate: number | null;
  failures: number;
  evictions: number;
  queue_depth: number;
  /** 優先度別の未開始ジョブ数 (WP-7開発パネル用)。 */
  queue_depths: {
    interactive: number;
    viewport: number;
    prefetch: number;
    overscan: number;
    background: number;
  };
  cache_bytes: number;
  p50_ms: number | null;
  p95_ms: number | null;
  p99_ms: number | null;
  paused: boolean;
  /** 開発用エラー注入の現在値 (WP-7)。devtoolsビルドのみbackendが返す
   *  (WP-0。通常releaseビルドのstatsにはこのフィールドが無い)。 */
  injection_mode?: "none" | "decode" | "cache_write";
  injection_rate?: number;
  /** ローカル画像プロトコル統計 (WP-4)。 */
  proto_requests: number;
  proto_ok: number;
  proto_bytes_sent: number;
  proto_p50_ms: number | null;
  proto_p95_ms: number | null;
  proto_p99_ms: number | null;
};

/** cancel_thumbnail_requests の戻り値 (WP-7でグリッドが使用)。 */
export type CancelThumbnailResult = {
  cancelled_queued: number;
  cancel_requested: number;
  not_found: number;
  skipped_unreadable: number;
};

/** set_failure_injection の戻り値。 */
export type InjectionStatus = {
  mode: "none" | "decode" | "cache_write";
  rate: number;
};

/** request_thumbnails / generate_all_thumbnails の戻り値。 */
export type RequestAccepted = {
  queued: number;
  rejected_dup: number;
  rejected_full: number;
  /** キュー満杯で繰延された件数 (generate_allのみ。バックログへ入り
   * 完了駆動で投入される。WP-16 F1)。 */
  deferred: number;
  cache_hits: number;
  skipped_unreadable: number;
  skipped_unknown: number;
  /** より新しいgenerate_all要求で置換され、この呼では未処理に終わった件数
   * (FG-WP16-5 I1 — 旧呼は新呼に取って代わられるのが正しい挙動。
   * 投入・繰延は行われない)。 */
  superseded: number;
};

/** thumb:ready イベントの項目。 */
export type ThumbReadyItem = {
  asset_id: string;
  size_class: string;
  cache_generation: number;
  /** 内容感応URLバージョン (FG-1)。`?v={version}` に使う。 */
  version: number;
};

/** thumb:failed イベントの項目。 */
export type ThumbFailedItem = {
  asset_id: string;
  size_class: string;
  reason: string;
};

/** サイズクラスのIPC文字列 (Rust: gento_core::SizeClass)。 */
export type SizeClassId = "s256" | "s512" | "preview2048";

/** 優先度のIPC文字列 (Rust: gento_core::Priority)。 */
export type PriorityId = "interactive" | "viewport" | "prefetch" | "overscan" | "background";
