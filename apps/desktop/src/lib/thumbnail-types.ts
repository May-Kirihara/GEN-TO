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
  cache_bytes: number;
  p50_ms: number | null;
  p95_ms: number | null;
  p99_ms: number | null;
  paused: boolean;
  /** ローカル画像プロトコル統計 (WP-4)。 */
  proto_requests: number;
  proto_ok: number;
  proto_bytes_sent: number;
  proto_p50_ms: number | null;
  proto_p95_ms: number | null;
  proto_p99_ms: number | null;
};

/** request_thumbnails / generate_all_thumbnails の戻り値。 */
export type RequestAccepted = {
  queued: number;
  rejected_dup: number;
  rejected_full: number;
  cache_hits: number;
  skipped_unreadable: number;
  skipped_unknown: number;
};

/** thumb:ready イベントの項目。 */
export type ThumbReadyItem = {
  asset_id: string;
  size_class: string;
  cache_generation: number;
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
