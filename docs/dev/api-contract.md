# GEN-TO IPC API契約 (Phase 2 / WP-2時点)

- 作成日: 2026-08-28 (WP-2)
- 最終更新: 2026-08-29 (WP-3: §7〜§11を追加)
- 目的: Tauri command・イベントの形状をWP-3〜6の契約基準として固定する。
- 整合性情報: 本書の型は `crates/gento-core/src/catalog.rs`、`apps/desktop/src-tauri/src/catalog.rs`、
  `apps/desktop/src/lib/catalog-types.ts`、およびサムネイル系は
  `crates/gento-core/src/jobs.rs`、`apps/desktop/src-tauri/src/thumbnails.rs`、
  `apps/desktop/src/lib/thumbnail-types.ts` の実装と一致する。
  変更時は本書と実装の両方を更新する。

## 0. 命名規約 (既知の非統一について)

| 系 | 命名 | 由来 |
|---|---|---|
| `ImageListItem` / `CatalogPage` (ページDTO) | camelCase | 本体計画 §9 のDTO型定義に合わせた |
| `CatalogSummary` / イベントペイロード | snake_case | WP-2委任仕様の表記に合わせた |
| `health` (WP-1) | snake_case | WP-1実装 |

混合を許容している理由: 両仕様がそれぞれ明記しており、WP-2で一方に寄せると他方の委任契約から
逸脱するため。統一は将来WPで両文書を更新して行う (本WPではスコープ外)。

TypeScript型の正本は `apps/desktop/src/lib/catalog-types.ts`。以下に全文を示す。

```ts
export type CatalogMode = "real" | "virtual";
export type ImageState = "ready" | "unreadable" | "unsupported" | "corrupt";
export type ThumbnailState = "missing" | "queued" | "ready" | "failed";

export type ImageListItem = {
  id: string;            // u64を10進文字列化 (JS安全整数範囲の問題を回避)
  displayName: string;
  width: number | null;
  height: number | null;
  byteSize: number;
  state: ImageState;
  thumbnailState: ThumbnailState;
};

export type CatalogPage = {
  generation: number;
  total: number;
  offset: number;
  items: ImageListItem[];
  has_more: boolean;
};

export type CatalogSummary = {
  generation: number;
  mode: CatalogMode;
  total: number;
  scanning: boolean;
  scanned_count: number;
  error_count: number;
  last_scan_elapsed_ms: number | null;  // 走査前はnull、走査中はライブ値
};

export type ScanProgressEvent = {
  scanned: number;      // 列挙したファイルエントリ累積 (拡張子を問わない)
  found: number;        // カタログへ追加したPNG累積
  errors: number;       // プローブ失敗 + walkdir/metadataエラー累積
  elapsed_ms: number;
  done: boolean;
  error_samples?: string[];  // 直近最大5件のみ (イベント嵐対策)
};

export type CatalogChangedEvent = {
  generation: number;
  reason: "scan_batch" | "scan_done" | "mode_switch";
};

export type CatalogPageError =
  | { kind: "staleGeneration"; requested: number; current: number }
  | { kind: "invalidRange"; message: string };
```

## 1. Commands

### `health` (WP-1から変更なし)

```ts
invoke<HealthInfo>("health")
```

### `get_catalog_summary`

```ts
invoke<CatalogSummary>("get_catalog_summary")
```

- 常に成功する (読み取りのみ)。
- `scanned_count` / `error_count` は走査中はライブ値、走査後は最終値。

### `get_catalog_page`

```ts
invoke<CatalogPage>("get_catalog_page", { generation: number, offset: number, limit: number })
```

- 成功: `CatalogPage`。
- 失敗: `CatalogPageError` をreject。
  - `staleGeneration`: `generation` がサーバ側と不一致。フロントは summary 再取得→同一オフセットで
    1回再試行する (`CatalogStore.loadPage` の実装参照)。
  - `invalidRange`: `limit` が 0 または 501以上 (制約§2参照)。
- `offset >= total` はエラーではなく空ページ (`items: []`, `has_more: false`)。

### `start_scan`

```ts
invoke<null>("start_scan")
```

- 失敗 (string reject): 走査中の二重開始 ` "scan already in progress"`。
- 成功時の効果:
  1. カタログ内容をクリアし、モードを Real へ戻す
  2. `generation` を +1
  3. バックグラウンドスレッドで走査開始 (UIスレッドでファイルI/Oは行わない)
- 走査は `--dataset` で指定したルートを再帰走査する。シンボリックリンクは辿らない。
- ルート不存在はエラー扱いにならず、走査エラー (error_count) として報告される。
- 応答後にフロントは `get_catalog_summary` を再取得すること (generationが変わるため)。
  `scan started` イベントは発火しない (start_scanの呼び出し元自身が理由を知っているため)。

### `set_catalog_mode`

```ts
invoke<CatalogSummary>("set_catalog_mode", { virtual: boolean })
```

- 失敗 (string reject):
  - 走査中: `"cannot switch catalog mode while scanning"`
  - 実カタログ空に仮想へ切替: `"real catalog is empty; run start_scan before switching to virtual"`
- 成功時の効果 (モードが実際に変化した場合のみ):
  - `records` を仮想50,000件 / 実レコードへ差し替え、`generation` を +1、`catalog:changed`
    (reason `mode_switch`) をemitし、新 `CatalogSummary` を返す。
- 同一モードへの切替は no-op: generation不変・イベントなし・現在のsummaryを返す (冪等)。

## 2. Events

### `scan:progress` — 走査進捗

バッチ (64件) ごとと走査完了時にemit。payload は `ScanProgressEvent`。

- 走査完了前でも取得済み項目を表示できるよう、`catalog:changed` と同時に発火する。
- 個別エラーは毎件流さない。`errors` (累積件数) と `error_samples` (直近最大5件) のみ。

### `catalog:changed` — カタログ内容変化

payload は `CatalogChangedEvent`。

| reason | タイミング | generation |
|---|---|---|
| `scan_batch` | 走査バッチ追記ごと | 不変 (走査開始時のまま) |
| `scan_done` | 走査完了時 | 不変 |
| `mode_switch` | モード切替成立時 | +1 済みの新generation |

フロント (`CatalogStore.applyChanged`) は、イベントの `generation` が自scopeのsummaryと
不一致なら summary を再取得し、その後現在オフセットでページを再取得する。

## 3. generation semantics

- 初期値: 1 (空カタログ)。
- increment されるのは次の2箇所のみ:
  1. `start_scan` 受理時 (カタログ内容のリセットを伴うため)
  2. `set_catalog_mode` でモードが実際に変化した時
- 走査バッチ・走査完了では変化しない (走査中のページ要求がstaleにならないようにするため)。
- staleの扱い: `get_catalog_page` は不一致を `staleGeneration` エラーとして拒否。
  フロントはエラー種別を判定可能 (`kind` フィールド) で、summary再取得→再試行 (1回) する。
- ページ応答・要求の破棄: フロントは要求発行時のgenerationと応答のgenerationが一致しない
  場合、およびより新しい要求を発行済みの場合に応答を破棄する。

## 4. 制約・保証

1. **limit上限**: `1..=500` をサーバ側で強制 (`gento_core::catalog::MAX_PAGE_LIMIT`)。
   5万件カタログでも1要求あたり最大500件で、一括転送は発生しない。
2. **offset**: `offset >= total` は空ページ。`total` はモード切替でしか縮小せず、その時は
   generationも変わるため実害はない。
3. **安定順序**: 実カタログはパス文字列のUTF-8バイト昇順。非UTF-8パスは lossy 変換後で比較。
   **走査中は「その時点のソート済み」を返す**。途中バッチの挿入により、同一オフセットの
   内容が前後する可能性は許容する (进度に応じてフロントが再取得する)。
4. **仮想カタログ**: 実走査 (レコード1件以上) の後にのみ生成できる。規模は50,000件固定。
   - ID: 実レコード (ソート済み) の最後のID + 1 からの連番
   - 表示名: `virtual-{index:05}-{元表示名}` (indexは0開始)
   - rating = index % 6、triage = index % 3 (0=undecided, 1=keep, 2=reject) で決定的に変化
   - source_path・寸法・容量・mtimeは元レコードの循環参照 (実ファイルを複製しない)
5. **IDの寿命**: AssetIdはセッション内有効。再スキャンで採番し直す (walk順依存)。
   WP-3のキャッシュキーはIDではなく path+size+mtime を使う想定。
6. **走査の個別エラー隔離**: walkdirエラー (権限等)・プローブ失敗 (Unreadable/Corrupt) は
   1項目分のみとして記録し、走査全体は継続する。失敗項目も `state` 付きでカタログへ列挙される。
7. **実行スレッド**: 走査・プローブはバックグラウンドスレッド (`std::thread`)。command本体は
   ロック取得とメモリ操作のみで、UIスレッドでファイルI/Oを行わない。
8. **権限**: 既定capability (`core:default`) のみで動作する (event listen込み)。追加権限不要。

## 5. 計測 (acceptance「走査時間とメモリを記録できる」)

- イベント: `scan:progress.elapsed_ms` (ライブ) および完了時の最終値。
- ログ (tracing, stdout): 走査完了時に次の1行が出る。
  `scan metrics root=... found=1615 errors=0 scan_elapsed_ms=... probe_elapsed_ms=... rss_kb=...`
  - `rss_kb` は `/proc/self/status` の VmRSS (走査終了時点)。
- `GENTO_LOG` 環境変数でログレベルを制御 (README参照)。

## 6. WP-2委任仕様からの逸脱 (設計判断の記録)

| # | 逸脱 | 理由 |
|---|---|---|
| 1 | `scan_pngs` のsinkは `Vec<ScannedFile>` ではなく `ScanBatch { entries_walked, files }` を渡す | 進捗イベントの `scanned` (列挙エントリ累計) と `found` (PNG追加累計) を live に区別するため。委任仕様のままでは両者が常に同値になり進捗表示が冗長になる |
| 2 | `scan:progress` に `error_samples` (直近最大5件) を追加 | 委任仕様の「直近数件のサンプルのみ (イベント嵐対策)」を運ぶチャネルがイベントしかないため |
| 3 | `offset >= total` をエラーにせず空ページ | 仮想スクロールとtotal変化の競合で無害化できるため。エラーにする利益がない |
| 4 | 同一モードへの `set_catalog_mode` はno-op (generation不変) | 冪等性。不要なgeneration更新でUIのページを無駄に破棄させないため |
| 5 | `start_scan` はモードをRealへ戻しカタログをクリア | 仮想レコードの上に実走査結果が混在する状態を作らないため |
| 6 | ページDTOはcamelCase、summary/イベントはsnake_caseの混在 | §0 参照 (両仕様の明記に従った) |

---

# サムネイル生成系 (WP-3 / Phase 4 追加分)

§7以降はWP-3で追加した契約。命名はWP-2のsummary・イベント系規約に同じ
(snake_case。コマンド引数のみTauri規約によりJS側camelCase)。

## 7. 共通文字列型

- サイズクラス (`SizeClass`): `"s256" | "s512" | "preview2048"`
  (長辺256/512/2048px。`gento_core::jobs::SizeClass`)
- 優先度 (`Priority`): `"interactive" | "viewport" | "prefetch" | "overscan" | "background"`
  (この順に低優先。`gento_core::jobs::Priority`)
- 不正な文字列はcommandがstringでrejectする。

キャッシュキーは「AssetIdではなく」canonicalパス + byte_size + mtime_ms +
サイズクラス + アルゴリズムバージョン (`gento-image/src/cache.rs`)。
同一実ファイルを参照する仮想カタログ項目は同一キーにdedupされる。

## 8. Commands

### `request_thumbnails`

```ts
invoke<RequestAccepted>("request_thumbnails", {
  ids: string[], sizeClass: SizeClassId, priority: PriorityId
})
```

- AssetIdをカタログから解決し、キャッシュlookup → ヒットは即時 `thumb:ready`
  (バッチングされる)、ミスはワーカープールへ投入 (同一キーは1ジョブに統合)。
- 戻り値 `RequestAccepted`:

```ts
type RequestAccepted = {
  queued: number;          // 新規投入
  rejected_dup: number;    // 既存ジョブへ統合 (完了時にまとめて通知される)
  rejected_full: number;   // キュー満杯 (4096) で拒否。UIは再要求しない設計
  cache_hits: number;      // キャッシュ済みのため即時ready
  skipped_unreadable: number; // state != ready のレコード
  skipped_unknown: number;    // カタログに存在しないID
};
```

- `cache_hits` は委任仕様の3欄に加えた拡張 (即時ヒット経路の観測用)。
  後方互換 (追加フィールド) のため逸脱扱いとはしない。

### `reprioritize_thumbnails`

```ts
invoke<{ updated, not_updated, not_found, skipped_unreadable }>(
  "reprioritize_thumbnails", { ids: string[], sizeClass, priority })
```

- 未開始ジョブの優先度更新。`not_updated` は実行中または同一優先度。

### `cancel_thumbnail_requests`

```ts
invoke<{ cancelled_queued, cancel_requested, not_found, skipped_unreadable }>(
  "cancel_thumbnail_requests", { ids: string[], sizeClass })
```

- 未開始=即時除去 (イベントは出ない)。実行中=協調キャンセル要求
  (ジョブがデコード前/リサイズ前/エンコード前で検査)。

### `generate_all_thumbnails`

```ts
invoke<RequestAccepted>("generate_all_thumbnails", { sizeClass: SizeClassId })
```

- 表示中カタログ全体を `background` 優先度で一括登録 (受け入れ条件2/5の検証用)。

### `get_thumbnail_stats`

```ts
invoke<ThumbnailStats>("get_thumbnail_stats")
```

```ts
type ThumbnailStats = {
  generated: number;
  hits: number;
  misses: number;
  hit_rate: number | null; // hits/(hits+misses)。データなしはnull
  failures: number;
  evictions: number;       // 2GiB上限によるLRU退避
  queue_depth: number;     // 未開始ジョブ数
  cache_bytes: number;     // 最終ファイル+生成中一時ファイルの会計
  p50_ms: number | null;   // 生成時間 (データなしはnull)
  p95_ms: number | null;
  p99_ms: number | null;
  paused: boolean;
};
```

- 常に成功する (読み取りのみ)。

### `pause_thumbnail_generation`

```ts
invoke<null>("pause_thumbnail_generation", { paused: boolean })
```

- 一時停止中は新規ジョブを開始しない。実行中ジョブは完了まで継続。

### `clear_thumbnail_cache`

```ts
invoke<{ removed_files: number, removed_bytes: number, cache_generation: number }>(
  "clear_thumbnail_cache")
```

- 待機中ジョブを全キャンセルしてからキャッシュを全削除し、
  `cache_generation` を +1 する。実行中ジョブのコミットが追いつく場合、
  クリア後に正当なサムネイルが再登録されることがある (内容はキーに対応
  するため無害)。
- `cache_generation` はWP-4/5のプロトコルURL `?v=` で使用する。

## 9. Events

### `thumb:ready` — 生成完了・キャッシュヒット

- バッチング: 100ms窓 or 最大20項目ごとにemit。
- payload:

```ts
type ThumbReadyPayload = { items: ThumbReadyItem[] };
type ThumbReadyItem = {
  asset_id: string;        // 要求時のAssetId (10進文字列)
  size_class: string;      // 要求時のサイズクラスIPC名
  cache_generation: number; // 要求時点のgeneration
};
```

- キャッシュヒット (即時ready) も同じイベントで流れる。
- キャンセルされたジョブはこのイベントを出さない。

### `thumb:failed` — 生成失敗

- payload: `{ items: { asset_id, size_class, reason }[] }`
  (reasonは `ThumbnailError` の表示文字列。パスは含まれない)。

## 10. cache_generation semantics

- 初期値1。`clear_thumbnail_cache` 成功時のみ +1。
- readyイベントの `cache_generation` は**要求時点**の値 (クリア前の要求が
  クリア後に完了した場合、古いgeneration付きで届く = UIは破棄してよい)。

## 11. 制約・保証 (WP-3)

1. **UIスレッド**: 生成は固定ワーカープール (min(4, available_parallelism))。
   command本体はロックとメモリ操作のみ。
2. **dedup**: 同一キャッシュキー (パス+size+mtime+size_class+バージョン) の
   同時要求は1ジョブに統合し、完了時に全要求者へ通知。
3. **キュー上限**: 4096件 (未開始)。満杯時は `rejected_full` で拒否
   (バックプレッシャー。無制限spawnなし)。
4. **キャッシュ不変条件**: 最終ファイル+生成中一時ファイルの合計 ≤ 2GiB。
   書き込み成功ごとと起動時に検査、超過時は目標1.9GiB (95%) までLRU削除。
   実行中ジョブのキーと一時ファイルは削除禁止。アクセス台帳はメモリ内
   (プロセス再起動で未登録ファイルはmtime順に扱われる)。
5. **アトミック配置**: 一時ファイル `.tmp-{pid}-{seq}.webp` を最終ディレクトリと
   同一場所に作成 → fsync → rename。途中ファイルがhitすることはない。
6. **panic隔離**: ジョブのpanicは失敗扱い (`thumb:failed`) でワーカーは継続。
7. **シャットダウン**: `RunEvent::Exit` で待機ジョブをキャンセル完了化してから
   ワーカーをjoinする。
8. **権限**: 追加なし (`core:default` のみ)。
9. **キャッシュディレクトリ**: `--cache-dir <PATH>` 起動引数。既定は
   `dirs::cache_dir()/gento-spike/thumbnails`。
10. **ページDTOの `thumbnailState`**: WP-3では常に `"missing"` のまま
    (実状態はイベントとstatsで観測する。WP-4/5でURL表示に切り替える際に更新)。
