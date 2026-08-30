/**
 * カタログ系IPCのDTO・イベント型 (docs/dev/api-contract.md と一对一で対応)。
 *
 * 命名規約の注意: ImageListItem は本体計画 §9 のDTO定義に合わせ camelCase。
 * summary・イベントペイロードはWP-2委任仕様の表記に合わせ snake_case。
 */

export type CatalogMode = "real" | "virtual";

export type ImageState = "ready" | "unreadable" | "unsupported" | "corrupt";

export type ThumbnailState = "missing" | "queued" | "ready" | "failed";

/** get_catalog_page の項目DTO (Rust: src-tauri catalog::CatalogItemDto)。 */
export type ImageListItem = {
  /** u64を10進文字列化したもの (JS安全整数範囲の問題を回避)。 */
  id: string;
  displayName: string;
  width: number | null;
  height: number | null;
  byteSize: number;
  state: ImageState;
  thumbnailState: ThumbnailState;
  /** 内容感応URLバージョン (FG-1)。元ファイルのpath+size+mtimeと
   *  アルゴリズムバージョンから決まる。original表示のURLなどに使う。 */
  version: number;
};

/** get_catalog_page の戻り値 (Rust: gento_core::catalog::PageResult)。 */
export type CatalogPage = {
  generation: number;
  total: number;
  offset: number;
  items: ImageListItem[];
  has_more: boolean;
};

/** get_catalog_summary / set_catalog_mode の戻り値。 */
export type CatalogSummary = {
  generation: number;
  mode: CatalogMode;
  total: number;
  scanning: boolean;
  scanned_count: number;
  error_count: number;
  last_scan_elapsed_ms: number | null;
};

/** scan:progress イベントペイロード。 */
export type ScanProgressEvent = {
  /** 列挙したファイルエントリ累積 (拡張子を問わない)。 */
  scanned: number;
  /** カタログへ追加したPNG累積。 */
  found: number;
  errors: number;
  elapsed_ms: number;
  done: boolean;
  /** 直近数件のエラーサンプル。 */
  error_samples?: string[];
};

/** catalog:changed イベントペイロード。 */
export type CatalogChangedEvent = {
  generation: number;
  reason: "scan_batch" | "scan_done" | "mode_switch";
};

/** get_catalog_page のエラー (Rust: gento_core::catalog::PageError)。 */
export type CatalogPageError =
  | { kind: "staleGeneration"; requested: number; current: number }
  | { kind: "invalidRange"; message: string };
