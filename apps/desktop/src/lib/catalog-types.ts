/**
 * カタログ系IPCのDTO・イベント型 (docs/dev/api-contract.md と一对一で対応)。
 *
 * 命名規約の注意: ImageListItem は本体計画 §9 のDTO定義に合わせ camelCase。
 * summary・イベントペイロードはWP-2委任仕様の表記に合わせ snake_case。
 */

/** WP-2で "library" (登録ルートのDB由来カタログ) を追加。 */
export type CatalogMode = "real" | "virtual" | "library";

/** 表示ソート (WP-4。Rust: gento_core::catalog::CatalogSort)。 */
export type CatalogSort = "name" | "date_desc" | "size_desc";

export type ImageState = "ready" | "unreadable" | "unsupported" | "corrupt";

export type ThumbnailState = "missing" | "queued" | "ready" | "failed";

/** 項目のDB上の位置づけ (WP-4)。情報パネルの登録状態表示に使う。 */
export type ItemDbState = "asset" | "provisional" | "adhoc";

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
  /** アニメーション付き画像か (WP-3。ビューアはoriginalを直接表示)。 */
  isAnimated: boolean;
  /** 内容感応URLバージョン (FG-1)。元ファイルのpath+size+mtimeと
   *  アルゴリズムバージョンから決まる。original表示のURLなどに使う。 */
  version: number;
  /** 実ファイルの絶対パス (WP-4。情報パネル・OS連携)。 */
  path: string;
  /** 更新日時 (Unixエポックms。WP-4)。 */
  mtimeMs: number;
  /** プローブ由来MIME (未プローブはnull。WP-4)。 */
  mimeType: string | null;
  /** DB状態 (WP-4)。asset=登録済みAsset確定 / provisional=登録済みハッシュ待ち
   *  / adhoc=未登録 (一時走査・仮想)。 */
  dbState: ItemDbState;
  /** 選別状態 (WP-4。セッションの採用/除外カウント用。WP-5で編集接続)。 */
  triage: "undecided" | "keep" | "reject";
  /** 星評価 0..=5 (WP-5。グリッドセルの星バッジ表示)。 */
  rating: number;
  /** お気に入り (WP-5。グリッドセルの♥バッジ表示)。 */
  favorite: boolean;
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
  /** 現在の表示ソート (WP-4)。 */
  sort: CatalogSort;
};

/** scan:progress イベントペイロード (WP-2でライブラリスキャン用フィールド追加)。 */
export type ScanProgressEvent = {
  /** 列挙したファイルエントリ累積 (拡張子を問わない)。 */
  scanned: number;
  /** カタログへ追加したPNG累計 / ライブラリスキャンでは新規発見累計。 */
  found: number;
  errors: number;
  elapsed_ms: number;
  done: boolean;
  /** 直近数件のエラーサンプル。 */
  error_samples?: string[];
  /** 以下はライブラリスキャンのみ (一時走査では 0/undefined)。 */
  updated?: number;
  relocated?: number;
  missing?: number;
  hashed?: number;
  pending_hash?: number;
  phase?: "scanning" | "hashing" | null;
};

/** catalog:changed イベントペイロード。 */
export type CatalogChangedEvent = {
  generation: number;
  reason: "scan_batch" | "scan_done" | "mode_switch" | "sort_change";
};

/** get_catalog_page のエラー (Rust: gento_core::catalog::PageError)。 */
export type CatalogPageError =
  | { kind: "staleGeneration"; requested: number; current: number }
  | { kind: "invalidRange"; message: string };
