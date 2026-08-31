# GEN-TO IPC API契約 (Phase 2 / WP-2時点)

- 作成日: 2026-08-28 (WP-2)
- 最終更新: 2026-08-30 (WP-0: §0/§1 healthへ `api_version` 追加・§14 devtools feature化・§15 APIバージョン互換方針追加 / WP-1: §1 healthへ `db_open` 追加・§16 `db_status` 追加 / WP-2: §17〜21 登録ルート・永続カタログ・`set_catalog_source`・`"library"`モード追加 / **WP-3 (MVP): §22〜24 形式拡張・Orientation・アニメーション・透過背景・`isAnimated`フィールド追加**)
- 目的: Tauri command・イベント・ローカル画像プロトコルの形状をWP-3〜6の契約基準として固定する。
- 整合性情報: 本書の型は `crates/gento-core/src/catalog.rs`、`apps/desktop/src-tauri/src/catalog.rs`
  (ページDTOのversion拡張もここ)、
  `apps/desktop/src/lib/catalog-types.ts`、およびサムネイル系は
  `crates/gento-core/src/jobs.rs`、`apps/desktop/src-tauri/src/thumbnails.rs`、
  `apps/desktop/src/lib/thumbnail-types.ts`、プロトコル系は
  `apps/desktop/src-tauri/src/protocol.rs`、`apps/desktop/src/lib/protocol.ts` の実装と一致する。
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
  version: number;       // 内容感応URLバージョン (FG-1。§12の ?v= 仕様参照)
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

### `health` (WP-0で `api_version`、WP-1で `db_open` を追加)

```ts
invoke<HealthInfo>("health")
```

```ts
type HealthInfo = {
  app_name: string;
  version: string;
  /** IPC DTO契約のAPIバージョン (§15。Rust: gento_core::GENTO_API_VERSION)。 */
  api_version: number;
  dataset_path: string | null;
  dataset_exists: boolean;
  /** SQLite DBを開けているか (WP-1)。詳細は §16 `db_status`。 */
  db_open: boolean;
  timestamp_ms: number;
};
```

- 常に成功する (読み取りのみ)。`api_version`・`db_open` は必須フィールド
  (各追加WPより前の形状はフロントの検証でerror扱い)。

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

- 成功: `CatalogPage` (項目は `ImageListItem` + `version`。Rust側は
  `apps/desktop/src-tauri/src/catalog.rs::CatalogItemDto` — gento-coreのDTOに
  thin adapter層で内容感応versionを付与する)。
- 失敗: `CatalogPageError` をreject。
  - `staleGeneration`: `generation` がサーバ側と不一致。フロントは summary 再取得→同一オフセットで
    1回再試行する (`CatalogStore.loadPage` の実装参照)。
  - `invalidRange`: `limit` が 0 または 501以上 (制約§2参照)。
- `offset >= total` はエラーではなく空ページ (`items: []`, `has_more: false`)。

### `start_scan`

```ts
invoke<null>("start_scan")
```

- 成功時の効果:
  1. カタログ内容をクリアし、モードを Real へ戻す
  2. `generation` を +1
  3. バックグラウンドスレッドで走査開始 (UIスレッドでファイルI/Oは行わない)
- **走査中の再投入も成功する** (QA-WP4): 旧走査は世代トークンで無効化され、
  旧ワーカーの結果は破棄される。中止する場合は `cancel_scan`。
- 走査は `--dataset` で指定したルートを再帰走査する。シンボリックリンクは辿らない。
  深さ上限16・画像200,000件で打ち切り (QA-WP4ガード)。
- ルート不存在はエラー扱いにならず、走査エラー (error_count) として報告される。
- 応答後にフロントは `get_catalog_summary` を再取得すること (generationが変わるため)。
  `scan started` イベントは発火しない (start_scanの呼び出し元自身が理由を知っているため)。

### `set_catalog_mode`

```ts
invoke<CatalogSummary>("set_catalog_mode", { virtual: boolean })
```

- 失敗 (string reject。devtoolsビルドのみ):
  - 走査中: 「走査中はカタログモードを切替できません」
  - 実カタログ空に仮想へ切替: 「実カタログが空です。仮想カタログへ切替する前にスキャンを実行してください」
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
  // ローカル画像プロトコル統計 (WP-4で追加)
  proto_requests: number;      // プロトコルハンドラが処理した要求 (拒否含む)
  proto_ok: number;            // 200応答件数
  proto_bytes_sent: number;    // 200応答で転送したバイト数
  proto_p50_ms: number | null; // プロトコル応答時間 (データなしはnull)
  proto_p95_ms: number | null;
  proto_p99_ms: number | null;
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
- `cache_generation` はreadyイベントの古い世代を破棄する**世代ガード**に
  使う (URLには内容感応versionを使う。FG-1で分離。§10参照)。

## 9. Events

### `thumb:ready` — 生成完了・キャッシュヒット

- バッチング: 100ms窓 or 最大20項目ごとにemit。
- payload:

```ts
type ThumbReadyPayload = { items: ThumbReadyItem[] };
type ThumbReadyItem = {
  asset_id: string;        // 要求時のAssetId (10進文字列)
  size_class: string;      // 要求時のサイズクラスIPC名
  cache_generation: number; // 要求時点のgeneration (世代ガード用)
  version: number;         // 内容感応URLバージョン (FG-1。?v= に使う)
};
```

- キャッシュヒット (即時ready) も同じイベントで流れる
  (versionは要求時のキャッシュキー入力から計算するためヒット・生成で同一)。
- キャンセルされたジョブはこのイベントを出さない。

### `thumb:failed` — 生成失敗

- payload: `{ items: { asset_id, size_class, reason }[] }`
  (reasonは `ThumbnailError` の表示文字列。パスは含まれない)。

## 10. cache_generation・version semantics

### cache_generation (世代ガード用)

- 初期値1。`clear_thumbnail_cache` 成功時のみ +1。
- readyイベントの `cache_generation` は**要求時点**の値 (クリア前の要求が
  クリア後に完了した場合、古いgeneration付きで届く = UIは破棄してよい)。
- URLには使わない (後述のversionと独立。FG-1で分離)。

### version (内容感応URLバージョン、FG-1)

- 各アセットの version = キャッシュキー入力と同じ
  **canonicalパス + byte_size + mtime_ms + アルゴリズムバージョン** の
  Xxh3-128下位56bit (`gento-image/src/cache.rs::content_version`。
  56bit ⇒ 最大17桁で、クエリ `v` の18桁上限に常に収まる)。
- **サイズクラスは入力に含めない** (設計判断): URLはパス側
  (`/asset/{id}/{s256|s512}`) でクラスを区別するため、versionは
  (パス, サイズ, mtime, アルゴリズム) の変化だけで全スキームのURLを
  変化させれば足りる。クラスを含めるとDTOがクラス別の値を運ぶ必要があり
  過剰になる。
- グローバル `cache_generation` との合成はしない (設計判断):
  version単体で内容を識別でき、キャッシュクリア後の再生成は同一URLになる
  (内容が同一のためimmutableキャッシュの再利用は正当)。
- 元ファイル変更 → サーバのキャッシュキー変化 → readyイベント/ページDTOの
  version変化 → URLの `?v=` 変化 → WebViewのimmutableキャッシュが無効化
  (§12-5「URLが一意なら内容不変」の受け入れを満たす)。
- 出所: `thumb:ready` イベント (thumb/preview表示の切り替え时机) と
  `get_catalog_page` の項目DTO (original表示など生成を待たない用途)。

## 11. 制約・保証 (WP-3)

1. **UIスレッド**: 生成は固定ワーカープール (min(4, available_parallelism))。
   command本体はロックとメモリ操作のみ。
2. **dedup**: 同一キャッシュキー (パス+size+mtime+size_class+バージョン) の
   同時要求は1ジョブに統合し、完了時に全要求者へ通知。
3. **キュー上限**: 4096件 (未開始)。満杯時は `rejected_full` で拒否
   (バックプレッシャー。無制限spawnなし)。
4. **キャッシュ不変条件**: 最終ファイル+生成中一時ファイルの合計 ≤ 2GiB。
    書き込み開始前に予定サイズをin-flight会計へ**予約**し (書き込み中の一時
    ファイルが会計から漏れない。FG-1)、予約時と書き込み成功ごとと起動時に
    検査、超過時は目標1.9GiB (95%) までLRU削除。退避しても抑えられない
    予約は巻き戻されて書き込みが失敗する (バックプレッシャー)。
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

---

# ローカル画像プロトコル (WP-4 / Phase 5 追加分)

## 12. カスタムURIスキーム

3つのプロトコルを `register_asynchronous_uri_scheme_protocol` で非同期登録する
(ファイルIOは `tauri::async_runtime::spawn_blocking`。プロトコル/UIスレッドをブロックしない)。

| scheme | URL形式 | 内容 | Content-Type | 上限 |
|---|---|---|---|---|
| `gento-thumb` | `//localhost/asset/{id}/{s256\|s512}?v={version}` | サムネイルのキャッシュWebP | `image/webp` | 64MiB |
| `gento-preview` | `//localhost/asset/{id}?v={version}` | preview2048キャッシュWebP | `image/webp` | 64MiB |
| `gento-original` | `//localhost/asset/{id}?v={version}` | 登録済みカタログ資産の元ファイル (MVP全形式。WP-3からContent-Typeはアセットごと) | 形式ごと (`image/png` / `image/gif` / `image/webp` 等。プローブ由来) | 512MiB |

`?v={version}` は**内容感応バージョン** (§10参照。元ファイルの
path+size+mtimeとアルゴリズムバージョンから決まる)。任意の資産で
「同一URL ⇒ 同一内容」が成り立つため `immutable` 1年キャッシュが安全であり、
元ファイルの変更はversionの変化経由でURLを変えてWebViewキャッシュを更新させる。

フロントエンドは `apps/desktop/src/lib/protocol.ts` のビルダ
(`thumbUrl` / `previewUrl` / `originalUrl`) を使う。ビルダは常に
`scheme://localhost/asset/...` 形式 (authority明示) を生成する。

### 検証規則 (実装: `protocol.rs::parse_protocol_request`)

URLのどの部分もファイルシステムパスとして解釈しない。解決は
AssetId → カタログレコード → キャッシュキー/元パス (アプリ保有レジストリ) のみ。

- メソッド: GETのみ。他 (HEAD/POST等) は **405**。
- scheme: ハンドラのschemeと完全一致 (不一致は400)。
- authority: userinfo・port禁止。hostは `localhost` (厳密一致。非特殊schemeの
  hostは大文字小文字を保存するため `LOCALHOST` は拒否) または不在のみ。
  他のhost (URL案の `asset` をauthorityに置いた形式、IP、`..` 等) は **400**。
- パス: セグメントは厳密に `asset` → ID → [サイズクラス (thumbのみ)]。
  IDは10進数字のみ・20桁上限・u64範囲。サイズクラスはenum完全一致
  (`s256`/`s512`。thumbでの `preview2048` は拒否)。過剰・欠損・空セグメントは400。
  パーセントエンコードは一切許可されない (正当なID/クラス構文に
  `%` が現れないため、`%2F` `%2E` `%5C` `%00` `%252E` はすべて構文検証で拒否。
  ただし `%2E%2E` はWHATWG URLパーサがドットセグメント正規化を行うため
  正規化後の形状検証で拒否される)。
- クエリ: なし、または `v={10進1..18桁}` のみ。未知クエリ (`x=1`)・
  複数・空値・`V=1` は **400**。
- fragment・opaque形式 (`scheme:asset/1`・cannot-be-a-base) は400。

### 解決とステータス

- 未登録ID・unreadableレコード・未生成 (キャッシュミス)・キャッシュファイル消失 →
  **404 即応** (生成を待ってブロックしない — スクロール中の大量要求を滞留させない)。
- サイズ上限超過 → **413**。その他の読み込みIOエラー → 500。
- キャッシュ解決: 最終コンポーネントは `symlink_metadata` で正規ファイル確認
  (シンボリックリンク拒否) + `canonicalize` でキャッシュルート包含検証 + サイズ上限。
  元ファイル (original) も同様にシンボリックリンク拒否。
- **TOCTOU閉鎖 (FG-1)**: 読み取りは最終コンポーネントを `O_NOFOLLOW` で開き、
  open済みfdのfstatで検証時statの (dev, ino) 一致と、`/proc/self/fd/{fd}` の
  実パスが正規キャッシュルート配下であることを再確認する
  (検証→open間のシンボリックリンク・中間ディレクトリ差し替えを拒否)。
  同一inodeへのhardlink差し替えのみ検出不能だが、キャッシュディレクトリへ
  書き込める攻撃者は内容を直接書き換え可能なため脅威モデル外。
- 不正要求 → 400/404/405。

### 応答ヘッダ

- 成功: `Content-Type` (正確) / `Content-Length` / `X-Content-Type-Options: nosniff` /
  `Cache-Control`。
- `Cache-Control`: `v` クエリ付きURLは `public, max-age=31536000, immutable`
  (versionが内容感応のため同一URLの内容は不変)、無しは `no-store`。
- エラー本文は固定の短い文字列 (`not found` 等)。**パス・ID・URLを含まない**。
  ログも scheme・理由kind・ID・ステータスのみ (到着URLのdebugログを除く)。

### 未生成→表示切替フロー

1. UIは `request_thumbnails` で生成を要求 (プロトコルURLは直接叩かない)
2. `thumb:ready` イベントの `version` を受け取る
3. `?v={version}` 付きURLを組み直して `<img src>` へ反映
   (original表示はページDTOの `version` を使う。生成不要)

### CSP (tauri.conf.json)

`img-src` のみ制限 (`'self' data: gento-thumb: gento-preview: gento-original:`)。
dev (`devCsp`) とrelease (`csp`) で同一。
**これは本プロトタイプのローカル画像プロトコルのための限定対応であり、
一般的なCSPハードニング (connect-src・script-src等の厳格化) を実施した
ものではない** (スパイクの範囲外。MVP前の課題)。connect-src等は既定
(無制限) を維持する。

### 統計

プロトコル応答時間 (p50/p95/p99) と転送バイト・件数を `ProtoStats`
(`AppState` 内) に記録し、`get_thumbnail_stats` の `proto_*` フィールドへ統合する。
durationサンプルは100,000件で打ち切り。

## 14. WP-7 (フェーズ8/9) で追加・拡張したAPI

> WP-0で本節の全command (と `set_catalog_mode`、仮想カタログモード) は
> **devtools機能**へ再分類された: `debug_assertions` または cargo feature
> `devtools` (releaseは `--features custom-protocol,devtools` で明示有効化) でのみコンパイル
> される。通常releaseビルドのinvoke handlerにこれらの識別子は存在しない
> (`scripts/release-smoke.sh` がバイナリレベルで検証する)。
> devtoolsビルド内では更に環境変数 `GENTO_DEV_BRIDGE` (値が**厳密に `"1"`**
> のときのみ有効) がランタイムゲートとなり、無効ならstringでrejectする。

### `set_failure_injection`

```ts
invoke<{ mode: string, rate: number }>("set_failure_injection", {
  mode: "none" | "decode" | "cache_write", rate: 0.0..1.0
})
```

- 開発用の決定的エラー注入。即時に全ワーカーへ反映される。
- **ブリッジ無効時はErr** (`"dev bridge is disabled..."`)。
- `rate` は [0,1] 外を**reject**する (`"rate must be within 0.0..=1.0"`。
  FG-1でクランプから変更 — 不正入力を黙って受け付けない)。
- **判定はassetIdではなくキャッシュキー (fingerprint由来) のハッシュ閾値**
  (60bitを1e6スケール閾値と比較)。ジョブはキャッシュキーでdedupされるため
  1ジョブが複数の仮想assetIdに対応し得るが、キャッシュキーは実ファイル+
  size+mtimeから決まるため決定性は同等。実カタログ (1ファイル=1 asset) では
  assetIdハッシュと同値になる。WP-7仕様からの意図的な逸脱として記録。
- `decode`: デコード前に `decode failed: injected failure (decode)` で個別failed。
- `cache_write`: 一時ファイル作成後に失敗 (Dropガードが掃除。`.tmp` 残存なし)。
  `cache commit failed: injected failure (cache_write)`。
- rate=0 / mode=none で完全復帰。

### `dev_bridge_enabled`

```ts
invoke<boolean>("dev_bridge_enabled")
```

- ブリッジ有効時は `Ok(true)`。**無効時はErr** (上記ゲート参照)。
- devtoolsビルドのreleaseで計測ブリッジ (devtest.ts) を有効化するための
  ランタイムフラグ (既定無効。`GENTO_DEV_BRIDGE` は値が厳密に `"1"` の
  ときのみ有効)。通常releaseビルドではこのcommand自体が存在しない。

### `devtest_report`

```ts
invoke<null>("devtest_report", { payload: string })
```

- 計測ブリッジから機械可読行を `devtest:` ターゲットのinfoログへ流す。
  bash側は `grep devtest:` で採取する。
- **ブリッジ無効時はErr** (ログへ書き込めない)。

### `get_thumbnail_stats` の拡張フィールド (後方互換の追加)

```ts
queue_depths: {   // 優先度別の未開始ジョブ数 (WP-7開発パネル用)
  interactive: number; viewport: number; prefetch: number;
  overscan: number; background: number;
};
injection_mode: "none" | "decode" | "cache_write";
injection_rate: number;
```

### グリッドの画面外キャンセルポリシー (フロント実装、WP-7 A1)

- `GridStore` は需要デバウンス毎 (150ms) に、維持ゾーン
  (可視∪進行方向prefetch∪overscan) の外へ出た現在サイズクラスのpending要求を
  `cancel_thumbnail_requests` へ送る。インデックス解決不能なIDは保守的に残す。
- 画面外へ出た失敗記録 (`FailureSet`) はリトラクトされ、再登場時の次の
  レンジ変化で再試行される (同一レンジ内の自動リトライはしない)。

### SIGTERM graceful終了 (Rust実装、WP-7 A4)

- SIGTERM/SIGINT → self-pipe (libc) → 監視スレッド → `AppHandle::exit(0)` →
  `RunEvent::Exit` → `ThumbnailService::shutdown()` (ワーカー排出・join・
  キューCancelled化)。ウィンドウ閉じと同じ経路。

## 15. APIバージョン互換方針 (WP-0)

- 定数: `gento_core::GENTO_API_VERSION` (現行 `1`)。`health` 応答の
  `api_version` フィールドでフロントエンドが判定する。
- **major** (破壊的): フィールド削除・リネイム・意味変更・列挙値の削除。
  この値を +1 する。フロントは `api_version` 不一致で「バックエンド互換なし」
  を表示して機能を落とす想定。
- **minor** (追加): フィールド・列挙値・command・イベントの追加。この値は
  変えない。フロントは未知のフィールドを無視する (health正規化の前方互換
  テスト参照)。
- devtoolsビルド限定のAPI (§14) はこのバージョニングの対象外
  (計測ドライバと同時に更新するため)。
- 互換方針の運用: 破壊的変更時はこの値・本契約書・`health.test.ts` を
  同時に更新する。

## 16. データベース状態 (WP-1)

### `health.db_open` フィールド (§1参照)

SQLite DBを開けていれば `true`。開けなかった場合もアプリは続行する
(通常ファイラー機能はDB非依存のため) ため、このフィールドでUIが状態を
判別する。

### `db_status`

```ts
invoke<DbStatusDto>("db_status")
```

```ts
type DbStatusDto = {
  path: string;                        // 解決済みDBファイルパス
  open: boolean;                       // DBを開いているか
  open_error: string | null;           // 開けなかった理由 (開いていればnull)
  schema_version: number | null;       // PRAGMA user_version (未開時null)
  journal_mode: string | null;         // "wal" 期待
  integrity_check: string | null;      // "ok" または問題要約
  foreign_key_violations: number | null; // PRAGMA foreign_key_checkの違反行数
  table_row_counts: Record<string, number> | null; // 12テーブルの行数
  db_bytes: number | null;             // DB本体のファイルサイズ
  wal_bytes: number | null;            // -walファイルのサイズ
  busy_timeout_ms: number | null;      // 既定5000
};
```

- 常に成功する (読み取りのみ)。productionビルドにも含まれる診断command
  (devtools対象外)。§14の計測系とは無関係。
- `open: true` でも `open_error` が入り得る (開いた後の診断失敗 —
  ディスク障害等。その場合は統計系フィールドが `null`)。
- `open_error` のエラー種別 (開けなかった理由・失敗した操作) は返すが、
  文字列中の絶対パスは `<db>` / `<path>` に伏せる。`path` フィールド
  自体はMVPのローカルwebview (信頼されたコンテンツ) 向けのため
  露出を許容する — リモート/非信頼コンテンツを載せる前に再検討。
- DBパスの解決順位: `--db <PATH>` 起動引数 > `GENTO_DB` 環境変数 >
  `AppDirs.data_dir/library.sqlite3` (テスト・計測用の上書き)。
- DBの並行性・backup仕様は [architecture.md](architecture.md) および
  `gento-db` crate doc参照。

---

# 登録ルート・永続カタログ (WP-2 / 2026-08-30 追加)

## 17. Commands

### `library_roots_list`

```ts
invoke<LibraryRoot[]>("library_roots_list")
```

```ts
type LibraryRoot = {
  id: string;             // 10進文字列 (AssetIdの文字列方針に合わせる)
  uuid: string;
  path: string;           // canonicalize済み絶対パス (lossy)
  enabled: boolean;
  watch_enabled: boolean;
  available: boolean;     // パスが現在ディレクトリ存在するか (アンマウント検出)
};
```

- DB未開時はstring reject (`"library feature unavailable..."`)。
- 常に成功するのはDB開時のみ。`available: false` のルートも返す
  (「不在ルート=無効表示、削除しない」方針)。

### `library_roots_add`

```ts
invoke<AddRootResponse>("library_roots_add", { path: string })
```

- 失敗 (string reject): 未存在パス (`"path does not exist..."`) /
  ディレクトリ以外 (`"path is not a directory"`) /
  同一パス登録済み (`"path is already registered"`)。
- 成功時の効果: canonicalizeしたパスでルート登録 (設定既定:
  watch_enabled=1) → 監視開始 → 対象ルートのスキャンを非同期投入 →
  `roots:changed` emit。
- **親子二重登録は警告であって拒否しない** (spec-draft §4.2の「警告または
  登録制限」から警告方式を採用):

```ts
type AddRootResponse = {
  root: LibraryRoot;
  overlapping: LibraryRoot[];  // 親子関係にある既存ルート (警告表示用)
};
```

### `library_roots_remove` / `library_roots_set_enabled`

```ts
invoke<null>("library_roots_remove", { rootId: string })
invoke<null>("library_roots_set_enabled", { rootId: string, enabled: boolean })
```

- `remove`: 監視停止 → ルート削除 (配下FileはCASCADE削除。**Assetは残る**
  — 評価等の共有メタデータ保持) → `roots:changed`。
  **orphan Assetの保持は設計上の挙動** (FG-WP2時点の明記): ルート解除後、
  どのFileからも参照されないAsset行 (と評価・タグ等) はDBに残り続ける。
  同一内容を再登録・他ルートで発見した際に統合 (メタデータ保持) される
  ため。明示的なGC (未参照Asset掃除) はWP-15で検討する。
- `set_enabled(false)`: 監視停止・スキャン対象外 (File行は保持)。
  `set_enabled(true)`: 監視再開 + 当該ルートのスキャン投入。
- 不正な `rootId` (非数字) は reject。

### `scan_start` / `scan_stop` / `scan_status`

```ts
invoke<null>("scan_start")            // 全有効ルートの再スキャン
invoke<null>("scan_stop")             // 実行中スキャン+ハッシュのキャンセル
invoke<LibraryScanStatus>("scan_status")
```

```ts
type LibraryScanStatus = {
  scanning: boolean;        // スキャンワーカー処理中
  phase: "scanning" | "hashing" | null;
  found: number; updated: number; relocated: number; missing: number;
  hashed: number; pending_hash: number; errors: number;
  watch_degraded: boolean;  // FG-WP2: watcher未確立の有効ルートあり (下記)
  hash_retry_pending: number;    // FG-WP2-2: 再提出待ちジョブ数 (下記)
  hash_retry_exhausted: number;  // FG-WP2-2: 再試行断念ファイル数 (下記)
};
```

- `watch_degraded` は「有効・監視ON・パス存在なのにwatcherが確立できて
  いないルートがある」場合にtrue (inotify上限到達等)。この状態でも
  メンテナンス周期 (60秒) のポーリング再スキャンで変更は拾われ続ける
  (即時性のみ劣化)。watcherの再確立も同じ周期で試行される。
- **ハッシュジョブの決定論リトライ (FG-WP2-2)**: queue満杯で拒否された
  ジョブとStale破棄 (世代トークン不一致) されたファイルは失われない。
  (a) プール完了による空き駆動の再提出、(b) メンテナンス周期 (60秒) での
  未リンクpresentファイル再提出、で決定論的に回収される。Staleの再試行は
  ファイルごとに8回を上限とし、到達したファイルは断念して
  `hash_retry_exhausted` へ露出する (次のスキャン = 新世代トークンの
  キューイングで再試行状態はリセットされる)。

- `scan_start` はワーカー処理中 (`scanning: true`) の呼び出しを
  「スキャンが既に進行中です」で拒否。既定ではUIから非同期で完了する。
- `scan_stop` は協調キャンセル (walk中止・不在化確定前の打ち切り・ハッシュ
  中止)。**再開 = `scan_start` 再呼び出し** (進行状況はDBが正本のため、
  差分スキャンが続きを冪等に拾う。ハッシュ待ちも `asset_id IS NULL` から
  再構成される)。
- `scan_status` は常に成功 (DB未開は初期値0のDTO)。
- 既存の `start_scan` (一時走査) とは別コマンド。名前の衝突なし。

### `set_catalog_source` (カタログ表示ソース切替)

```ts
invoke<CatalogSummary>("set_catalog_source", { source: "temp" | "library" })
```

- カタログの3ソース統合 (WP-2):
  - `temp`: `--dataset` 一時走査の結果 (従来のReal。未登録フォルダ閲覧)
  - `library`: 登録ルートのDB由来スナップショット (永続カタログ)
  - (virtual: devtools機能。`set_catalog_mode` のまま。Library表示中から
    の切替は「仮想カタログへ切替する前に一時走査へ戻してください
    (set_catalog_source)」で拒否)
- モードが実際に変化した場合のみ `generation` を +1 して `catalog:changed`
  (reason `mode_switch`) をemit。同一ソースへの切替はno-op。
- 一時走査の実行中 (`scanning`) は「走査中はカタログソースを切替できません」
  で拒否 (中止するには `cancel_scan`)。ライブラリスキャン中の切替は可
  (スナップショットは次の更新で上書きされる)。
- `CatalogMode` (`CatalogSummary.mode`) に `"library"` を追加 (列挙値追加 =
  minor。`GENTO_API_VERSION` は据え置き)。

## 18. Events

### `scan:progress` (拡張)

ライブラリスキャンも同じ `scan:progress` をemitする。既存フィールドに加え
**ライブラリスキャンのみ**以下が載る (一時走査では 0 / `null`):

```ts
type ScanProgressEvent = {
  scanned: number; found: number; errors: number;   // 既存
  elapsed_ms: number; done: boolean;                // 既存
  error_samples?: string[];                         // 既存
  updated?: number;      // 変更検出 (size+mtime変化)
  relocated?: number;    // 外部移動・改名の再発見 (os_file_id照合)
  missing?: number;      // 不在化
  hashed?: number;       // BLAKE3計算完了
  pending_hash?: number; // ハッシュ待ち残数
  phase?: "scanning" | "hashing" | null;
};
```

- ハッシュ進捗はイベント嵐回避のため100ms間隔で間引きされる。
- `done: true` はスキャン位相完了時 (ハッシュ残0) またはハッシュ完了時。

### `roots:changed`

ペイロードなし (`null`)。フロントは `library_roots_list` を再取得する。
発火: 追加・解除・有効切替・ルート不在検出。

## 19. ライブラリAssetIdの符号化 (設計判断の記録)

ライブラリカタログ (`source: "library"`) の一覧DTO `id` は **DB由来の安定ID**
(restart後も不変) とし、次の符号化を採る:

- **Asset確定済み (ハッシュ計算済み)**: `id = assets.id` (SQLite整数ROWID)。
  同一バイト内容の複数Fileは同一IDを返す (= 1 Asset共有が一覧から見える)。
- **ハッシュ未完了 (asset_id NULL)**: `id = 2^63 | files.id` の**仮ID**。
  bit63はSQLiteのINTEGER範囲外のため assets.id と衝突しない。
  ハッシュ完了でAssetへリンクされると安定IDへ切り替わる (フロントは
  `catalog:changed` → ページ再取得で追従。仮IDは過渡状態)。

WP-5のメタデータ編集は**安定ID (assets.id) を指定して**行うこと。
仮IDを編集対象にしないこと (過渡状態のため)。`AssetId` はIPCでは
10進文字列 (既存規約どおり)。

## 20. 監視プロトコル (WP-2。Codex計画レビュー指摘の反映)

```text
監視確立 (inotify再帰watch) → イベントバッファリング (400ms debounceで
パス単位結合) → 初回スキャン → キューイベント適用 (スコープ差分スキャン
= reconciliation) → (以後イベント駆動でスコープ差分)
```

- イベント結合規則 (gento-core `WatchBuffer`): 同一パスの Create+Modify →
  Modified、Remove+Create → Modified、変更後Remove → Removed (最終状態
  のみ残る)。inotifyのアクセスイベント (Access) は無視。
- 結合後の変化パスから影響ディレクトリを導出し、そのサブツリーのみ差分
  スキャン (`scope`)。スコープが24箇所を超えるかイベント欠落 (overflow・
  未知イベント種・watcherエラー) を検出した場合は**全体再スキャン**で収束。
  overflow (watcherエラー・未知イベント種を含む) は**通常イベントが1件も
  来なくても** debounceスレッドの50ms tickで検出され、全体再スキャンが
  発火する (FG-WP2修正)。
- スコープ差分スキャンはファイルシステムの最終状態のみに依存して冪等
  (世代トークン高速判定 + os_file_id移動検出 + 未参照missing化) のため、
  **大量イベント後も最終状態が一致する** (受入条件5。テスト
  `watcher_burst_of_many_events_converges` で実inotifyにより証明)。
- **不変世代トークン (FG-WP2-2)**: ファイル実体の同一性は
  `(os_file_id, size, mtime_ns)` — os_file_idはUnixでは (dev, ino)、
  mtime_nsはナノ秒精度 (migration #4の `files.mtime_ns` 列。旧 `mtime_ms`
  列は後方互換で残し走査は両方更新)。走査時にトークンを観測してfiles行へ
  保存 → ハッシュジョブがトークンを携帯 → ハッシュは**開いたfdのfstat**
  からトークンを取得 (パス再解決しない) し計算前後でfdトークン不変を検証
  → リンク時 (書き込みトランザクション内) でDB行の現在トークンと一致する
  場合のみリンク。不一致はStaleとして結果破棄・決定論リトライへ。これにより
  同パス再利用 (新inode) はinode差で、同一ms内の置換はns差で、それぞれ
  確実に検出される。同一inodeの移動・改名は同一バイトとして正しくリンク。
- **プローブのfd束ね (FG-WP2-2)**: PNG寸法プローブはハッシュと**同一の
  開いたfd**から読む (`hash_and_probe` 単一API。パス再オープン禁止)。
  ハッシュ対象バイトと異なる置換ファイルの寸法混入は構造的に不可能。
- **移動検出の内容検証 (FG-WP2-2強化)**: os_file_id (inode) 一致に加えて
  世代トークン全体 (size+mtime_ns含む) の一致を必須とする。不一致は移動
  扱いない新規登録+旧パスmissing化 (inode再利用・移動+内容変更から
  uuid/assetリンクを保護)。
- **在不在の積極的証明 (FG-WP2-2)**: 走査エラー対象パスの不在化可否は
  `Path::exists()` (権限・メタデータエラーでもfalse) でなく
  `symlink_metadata` で分類する: `Ok` = 存在 (読めなければblocking) /
  `Err(NotFound)` = 不在確定 (non-blocking) / `Err(その他)` = 判定不能
  (保守的にblocking)。権限エラーで既存サブツリーが誤ってmissing化
  しない。
- **不完全走査の保守的reconciliation (FG-WP2)**: 権限不足等で列挙に
  失敗したパス配下のレコードはmissing化しない (walkエラーは
  `scan_status.errors` と`error_samples`で警告として露出)。パス自体が
  消えたエラーは実際の不在のためmissing化をブロックしない。
- ルート自体の削除 (アンマウント含む) はwatcher解放 + `roots:changed`。
  DBのルート行・File行は保持。**ルート再出現時はメンテナンス周期 (60秒) で
  watcherを再確立し再スキャンする** (FG-WP2修正)。
- アプリ内からのスキャン中に到着したイベントはワーカーキューへ蓄積され、
  スキャン完了後に適用される (バッファリング)。

## 21. 逸脱・判断記録 (WP-2)

| # | 判断 | 理由 |
|---|---|---|
| 1 | ライブラリカタログのページ取得は「DB→スナップショット一括生成→既存ページAPI」という2段構成 (SQL直接ページングではない) | 既存のgeneration/protocol/thumbnail機構 (レコード解決はメモリ上) をそのまま再利用でき、WP-4 (3ペインUI) で直接SQLページングへ発展させられる。5万件規模のメモリ保持はスパイクの仮想カタログで実績済み |
| 2 | `scan:progress` イベントを一時走査とライブラリで共用 (フィールド追加) | UIの進捗表示が単一リスナーで済む。イベント分割は表示先が必要になった時点で検討 |
| 3 | 親子二重登録は警告レスポンス (`overlapping`) であって拒否しない | 仕様が「警告または登録制限」を選択可能のため。利用者判断を妨げない |
| 4 | 変更検出時 (size/mtime変化) は assetリンクを一度解除 (NULL化) し、再ハッシュで再リンク | 列追加なし (migration #2はindexのみの要件) で「内容変化 → 別Asset」を実現。同一内容なら同一Assetへ再リンクされメタデータは失われない |
| 5 | ハッシュ計算はサムネイルとは別の専用ワーカープール (2ワーカー) でBackground優先度 | gento-image WorkerPoolの優先度・キャンセル・一時停止機構を再利用しつつ、サムネイル生成 (Interactive/Viewport) をハッシュが飢餓させない分離。scan_stopで協調キャンセル |
| 6 | `overlapping_roots` の照合はcanonicalize後のパスで実施 (呼び出し側で正規化) | WP-1の設計 (字面比較・シンボリックリンク解決は呼び出し側) を踏襲 |
| 7 | **ハッシュの世代検証 (FG-WP2)**: 計算はopen済みfdから読み、計算前後の (size, mtime) を検証。リンクは書き込みトランザクション内でDBレコードの観測と突合し、一致時のみ実行 | 計算中の変更・rename/replace (inode差し替え)・「新しいスキャン後に到着した古いジョブ」を確実に破棄 (Stale) し、誤Assetリンクを防ぐ。寸法プローブも同一メタデータ検証に束ね、別読み不一致を排除 |
| 8 | **ハッシュ会計 (FG-WP2)**: `pending_hash` は「submit成功 (新規Enqueued) 数 − プール完了通知数」で厳密に維持。減算は完了通知ドリブンの会計スレッドが行う | dedup統合 (同一file_idジョブのマージ)・queue満杯放棄・キャンセル・pool shutdown放棄のすべてで増減が1:1に対応し、終了時に厳密に0になる (`hash_accounting_is_exact_under_dedup_queue_saturation_and_shutdown`) |
| 9 | **relocation失敗の伝播 (FG-WP2)**: `Duplicate`/`NotFound` は通常upsertへフォールバック (旧パス行はseen解除→不在化)。DB・制約エラーはスキャンエラーとして伝播・集計 | 失敗を黙って捨てると「移動でも不在化でもない」行が残る (Codex指摘) |
| 10 | **メンテナンス周期 (60秒) の導入 (FG-WP2)**: watcher再確立・不在ルートのwatcher解放・degraded時のポーリング再スキャンをワーカーのアイドル時に実行 | inotify上限・ルート再出現への劣化運用。新規スレッドを増やさずワーカーループのrecv_timeoutで実現 |

---

# MVP画像形式・表示パイプライン (WP-3 / 2026-08-30 追加)

## 22. 対応形式と形式判定

- 対象: PNG / JPEG / GIF / BMP / TIFF / WebP / AVIF (AVIFはAVIF-1で有効化。
  `image` crate `avif-native` feature = dav1d (システムライブラリ) +
  mp4parse。Linux検証済み、Windows/macOSはWP-17で実機確認。
  詳細は handoff 2026-08-30-mvp-wp3 のAVIF-1追記)
- **形式判定はmagic (先頭バイト) が正本**。拡張子は走査列挙と
  プローブ失敗時のMIMEフォールバックにのみ使用する
- プローブ (`gento_image::format`) はmagic+寸法+alpha+アニメーション情報
  (GIFフレーム数・WebP ANIM/ANMF走査) を返す。寸法上限
  (`MAX_DECODE_DIMENSION=20000`) を超えるヘッダは`Corrupt`
- AVIF固有の特性 (上流 `AvifDecoder` 実装に由来):
  - probe (寸法取得) はヘッダパースでなく**dav1dでのフルデコード**で
    行われる (軽量APIが無い)。寸法上限はデコード後検査。
  - `has_alpha` は**常にtrue** (color_typeが常にRgba8/Rgba16を返すための
    保守的な過大検出。decode結果のalphaは実payloadどおり)。
  - アニメーション判定はftypブランド (`avis`) のみ。フレーム数は持たない
    (`frame_count: null`)。ビューアはoriginal直接表示対象。
- 走査 (一時`temp`・登録ルート`library`両方) で全形式が列挙・プローブされ、
  `assets`行の `width/height/mime_type/has_alpha/is_animated/frame_count`
  へ保存 (schema #1の既存列のためmigration追加なし)
- magicが既知形式に一致しないファイルは `state: "unsupported"`、
  一致するが内容が不正 (切り詰め・破損・上限超過) は `"corrupt"`。
  いずれもグリッドでは既存の失敗セル表示

## 23. EXIF Orientation

- JPEG APP1 / TIFF / PNG eXIf / WebP EXIFチャンクからOrientation (1..8) を
  取得 (`kamadak-exif`)。**サムネイル・preview・ビューア表示で一貫して
  decode後に適用** (回転・反転)。元ファイルは一切変更しない
- 適用のため `ALGORITHM_VERSION` を **1→2** へ繰り上げ (§7のキャッシュキー・
  §10/§12のversionに織り込み済み。v1キャッシュはキー不一致で自然無効化)
- ICCプロファイル: MVPでは**色管理 (ICC適用) を行わない**。プロファイルの
  読み取り・維持は画像デコード基盤に委ね、表示は既定色空間扱い。
  対応範囲の拡大はWP-16以降の課題

## 24. アニメーション表示・透過背景

- **一覧サムネイル**: 常に静止画 (Rust側で先頭フレーム静止化。アニメGIF/WebP
  とも)。グリッドの要求は常に `s256|s512` のみ (`isAnimated`項目でも
  preview/original要求は発生しない — `grid-store.test.ts` で保証)
- **単一ビューア**: `isAnimated` 項目は**previewを経由せずoriginalを直接表示**
  (`gento-original://` + WebKit `<img>`ネイティブ再生)。昇格/復帰
  (`setOriginal`) とactual自動昇格はanimatedでは**スキップ**。originalの
  Content-Typeはプローブ由来の正確値 (例: `image/gif`)。
  original読み込み失敗時は静的previewへ復帰せずエラー表示 (再生喪失のため)
- **AVIFのoriginal表示はWebView能力依存 (既知制限)**: サムネイル・previewは
  Rust側dav1dデコードで常時動作するが、original (raw AVIFバイト) の描画は
  WebView自身のAVIF対応に依存する。検証環境のWebKitGTK 2.52.3
  (Ubuntu 24.04) はAVIF非対応のため、静止AVIFのactual/昇格はonerror→
  preview復帰、アニメAVIFのoriginal直接表示はエラー表示になる。
  WP-17で他環境の実態確認と対応判断を行う
  (handoff 2026-08-30-mvp-wp3 AVIF-1追記参照)
- **比較表示**: 静止画扱い (既存preview経路。アニメーション資産は先頭
  フレーム) — **既知制限**として明記。段階アニメーション比較は将来課題
- **透過背景**: ビューアで市松/白/黒を切替 (localStorage `gento.viewer.background`
  に永続化。製品設定体系はWP-15)

## 25. ページDTOへの追加フィールド (minor。`GENTO_API_VERSION` 据え置き)

```ts
type ImageListItem = {
  // ... 既存フィールド ...
  /** アニメーション付き画像か (WP-3)。ビューアのoriginal直表示判定。 */
  isAnimated: boolean;
};
```

- 一時`temp`カタログは走査プローブから、`library`カタログは`assets.is_animated`
  から供給される。未プローブ (ハッシュ待ちの仮ID) は `false`

# フォルダツリー・OS連携・UIシェル (WP-4 / 2026-08-30 追加)

## 26. Commands

### `fs_list_dirs` (WP-4)

任意パスの直下ディレクトリ列挙 (フォルダツリーの遅延ロード用)。

- 引数: `path: Option<String>` (`null` で起点ノード要求)
- 応答:

```json
{
  "path": "/home/user",
  "parent": "/home",
  "entries": [{ "name": "pictures", "path": "/home/user/pictures" }],
  "error": null
}
```

- 起点要求 (`path: null`): `entries` = [ホーム, `/`] (`name` は「ホーム」
  「ファイルシステム (/)」)。`path` は空文字、`parent` は null
- 隠しディレクトリ (`.` 始まり) は除外 (設定はWP-15)。シンボリックリンクは
  リンク先がディレクトリなら含む (壊れたリンクは除外)
- 名前は大文字小類を無視した昇順
- 不在・権限不足は `entries: []` + `error` に文言 (UIは「読み取り不能」表示)
- 親はパンくず/上位移動用 (`/` の親は null)

### `open_folder` (WP-4 / QA-WP4改訂)

閲覧フォルダを切り替えて一時走査 (adhoc) を開始する。

- 引数:
  - `path: String` — 既存ディレクトリの絶対パス
  - `sort: Option<"name" | "date_desc" | "size_desc">` (省略可。QA-WP4) —
    走査開始時点の表示ソート。フォルダ別保存ソートの再適用に使う
    (走査中は `set_catalog_sort` を使えないため開始時に反映する)
- 動作: 検証 (不在/非ディレクトリ/権限はエラー文言) → `dataset_path` 更新 →
  `start_scan` と同じ非同期走査を投入 (Real モードへ移行し内容を置換)
- 応答: 走査開始直後の `CatalogSummary` (`scanning: true`)
- **走査中でも成功する** (QA-WP4): 既存走査は世代トークンで無効化され、
  旧ワーカーの以降のバッチ・完了確定は破棄される (旧結果の混入なし。
  旧ワーカーは技術ログ `scan abandoned (superseded by newer scan)` を出す)
- 副作用: `catalog:changed` (scan_batch/scan_done) と `scan:progress` は
  `start_scan` と同一

### `cancel_scan` (QA-WP4)

実行中の一時走査 (adhoc) をキャンセルする。

- 引数: なし
- 動作: 走査トークンを無効化して走査ワーカーを中止 (walk打ち切り)。
  確定済みの部分結果はそのまま表示に残る (`scanning: false` に確定)
- 応答: 確定後の `CatalogSummary`。走査中でない場合もエラーにせず
  現在のsummaryを返す
- 副作用: キャンセル実行時のみ `scan:progress` (`done: true`) と
  `catalog:changed` (reason: `scan_done`) をemit
- 登録ルート (ライブラリ) 走査のキャンセルは既存 `scan_stop` を使う

### 一時走査の安全ガード (QA-WP4)

`start_scan` / `open_folder` の走査 (gento-image `scan_images_batch`) は:

- 深さ上限 16 (ルート相対。`/` 起点の際限ない再帰を防止)
- 画像列挙 200,000件で打ち切り (`ScanResult.truncated`。進捗の
  `error_samples` に「件数上限…打ち切りました」警告が反映される)
- 走査の再投入 (`open_folder` / `start_scan`) と `cancel_scan` は
  旧走査を協調キャンセルする (sinkがfalseを返しwalkを中止)

### `set_catalog_sort` (WP-4)

表示ソート切替。

- 引数: `sort: "name" | "date_desc" | "size_desc"`
- 動作: 表示レコードを並べ替え、generation を進めて `catalog:changed`
  (reason: `sort_change`) をemit。以後の走査バッチ・ソース切替にも適用
- 応答: 新 `CatalogSummary` (`sort` フィールドに現在値)
- 走査中は拒否 (フォルダ開設時のソート指定は `open_folder` の `sort` を使用)。
  同一ソート指定は無変化 (generation不変)

### `reveal_in_file_manager` / `open_with_default_app` (WP-4)

- 引数: `path: String` — 存在するファイル/ディレクトリ
- `reveal`: OSのファイルマネージャーで当該項目を選択表示
  (Linux: freedesktop FileManager1 D-Bus → `xdg-open <親>` フォールバック)
- `open`: 既定アプリケーションで開く
- 実装は `tauri-plugin-opener` のRust API (JS側plugin commandは使用しない
  ため capability 追加なし。パス存在検証は本command側で実施)
- 両者とも応答 `null` (エラー時は文字列)

## 27. 既存APIの変更 (WP-4、後方互換の追加)

### `get_catalog_page` 項目DTOの追加フィールド

```json
{
  "path": "/abs/to/img.png",
  "mtimeMs": 1748563200000,
  "mimeType": "image/png",
  "dbState": "asset | provisional | adhoc",
  "triage": "undecided | keep | reject"
}
```

- `dbState`: library表示ではAssetId符号化 (§17) から導出
  (`asset` = assets.id / `provisional` = bit63仮ID=ハッシュ待ち)。
  一時走査・仮想カタログは `adhoc` (DB管理外)
- `triage`: WP-2でDBに保持していた選別状態の暴露 (選別セッションの
  採用/除外カウント用。編集UIはWP-5)

### `get_catalog_summary` の追加フィールド

- `sort: "name" | "date_desc" | "size_desc"` — 現在の表示ソート

### `catalog:changed` のreason追加

- `"sort_change"` — `set_catalog_sort` による並べ替え (内容は不変)

## 28. UIシェルの状態保持 (WP-4。commandではなくWebView側)

- レイアウト (ペイン表示/幅/サムネイルサイズ): localStorage `gento:layout:v1`
- 前回フォルダ・ツリー展開: localStorage `gento:folders:v1`
  (起動時の走査対象は `--dataset` のまま決定論的。前回フォルダはツリーの
  選択ハイライトとして復元)
- **フォルダ別ソート** (QA-WP4): `gento:folders:v1` の `sorts`
  (キー=フォルダ絶対パス / ライブラリ表示は `"library"`。上限128エントリ)。
  フォルダ再オープン時に `open_folder` の `sort` 引数で再適用する
  (backendには状態を持たせない)
- スクロール位置: 既存 `gento:scroll:*` をフォルダ別キー
  (`real:<path>` / `library`) で運用
- 選別セッション: localStorage `gento:session:v1`
  (uuidセッション名・順序付きID列・位置・確認済み集合。採用/除外の値自体は
  DBのtriage由来)
- **セッションorigin詳細** (QA-WP4): `gento:session:v1` の `detail` —
  adhocセッションは `{kind: "adhoc-folder"|"adhoc-selection", folderPath, sort}`、
  ライブラリは `{kind: "library"}` を保存。再開時に現在のsource/フォルダ/
  ソートと一致しない場合は保存済みフォルダ+ソートを復元してからID解決する。
  復元後も件数ずれ (>5%) や現在位置のID解決失敗があれば警告表示して
  再開を中止する (adhocの走査連番IDはフォルダ+ソート+走査順の組でのみ
  安定なため。誤画像へのサイレント再開防止)

---

# 評価・選別・タグ・キャプション (WP-5 / 2026-08-31 追加)

## 29. Commands

メタデータ編集はすべて**stable AssetId (assets.id。§19)** を指定して行う。
IDは10進文字列 (AssetId IPC規約)。

### ガード (adhoc編集不可)

- カタログがLibraryモードでない (`get_catalog_summary.mode != "library"`) 場合、
  全編集コマンドは日本語エラー
  「未登録フォルダ表示中はメタデータを編集できません。登録ルートをライブラリとして表示してから編集してください」
  を返す (仕様§4.1「未登録はライブラリ機能なし」)。
  一時走査のIDは1開始連番でassets.idと数値が偶発一致し得るため、
  **モードガードが唯一の安全な判別**である (値だけでは区別できない)。
- 仮ID (bit63。ハッシュ計算待ち) を指定した場合も拒否
  (「ハッシュ計算待ちの項目は編集できません」)。UI側は
  `dbState === "asset"` で編集可否を判定して無効化する。
- 無効値 (rating 6以上・不明なcull/kind文字列) は日本語エラー。

### `asset_set_rating` / `asset_set_cull` / `asset_set_favorite`

```ts
invoke<null>("asset_set_rating", { ids: string[], rating: number })   // 0..=5
invoke<null>("asset_set_cull", { ids: string[], cull: "picked" | "rejected" | "unselected" })
invoke<null>("asset_set_favorite", { ids: string[], favorite: boolean })
```

- `ids` 一括対応。**1トランザクションで適用**し、途中の不在ID等で失敗した
  場合は全体ロールバック (一部だけ適用された状態を残さない — 受入条件5)。
- 除外 (`rejected`) は**ファイルを削除しない** (fs無操作。受入条件3)。
- 成功時 `asset:updated` イベント (下記) をemitし、表示済みカタログレコードを
  in-place更新する (generation不変 — ページ再取得不要)。

### `asset_tags_add` / `asset_tags_remove` / `asset_tags_list`

```ts
invoke<null>("asset_tags_add", { ids: string[], names: string[] })  // N asset × M tag
invoke<null>("asset_tags_remove", { assetId: string, tagId: string })
invoke<TagDto[]>("asset_tags_list", { assetId: string })
```

- `asset_tags_add` は名前からタグを**作成 or 既存統合**して付与する
  (手動追加: `source=manual`・`manually_edited=1`)。同一assetへの再付与は
  冪等 (行を重複させず `manually_edited=1` を再設定。`source` は初回由来を保持)。
- 付与も1トランザクション (一部asset不在でロールバック)。
- `TagDto`: `{ id: string, name: string, normalized_name: string,
  usage_count: number, manually_edited: boolean }` (camelCase)。

### `tag_suggest` / `tag_list` / `tag_delete`

```ts
invoke<TagDto[]>("tag_suggest", { prefix: string, limit?: number })  // 既定20・上限100
invoke<TagDto[]>("tag_list")
invoke<null>("tag_delete", { tagId: string })
```

- 候補は**正規化名の前方一致**・使用数降順 (同数は正規化名昇順)。
  `prefix` も正規化して照合 (大文字入力可)。LIKE特殊文字 (`%_\`) はエスケープ。
- `tag_delete` は**使用中のasset_tagsごと削除** (付随解除。ON DELETE CASCADE)。
  削除されたタグを付与されていたassetへむけ `asset:updated` (`changes:["tags"]`)
  をemitする。

### `asset_caption_get` / `asset_caption_set`

```ts
invoke<CaptionDto | null>("asset_caption_get", { assetId: string, kind: "description" | "sd_tags" | "user_note" })
invoke<CaptionDto>("asset_caption_set", { assetId: string, kind: ..., body: string })
```

- `CaptionDto`: `{ kind, body, source: "manual"|..., manually_edited: boolean,
  updated_at: string }`。
- 手動編集は「新行挿入 (`source=manual`・`manually_edited=1`・`current=1`) +
  旧現行行の `current=0` 化」で行う (履歴保持。data-model §5)。
  kindごとに現行は高々1行 (schemaの部分UNIQUE index)。
- `manually_edited=1` の行はWP-10のAI再生成で上書きされない
  (受入条件4「ユーザー編集済みキャプションを明確に識別」)。
- `ai_run_id` 列はWP-10でai_runsテーブル作成と同時にADD COLUMN予定
  (本WPでは構造のみ。source enumに `ai` を含む)。

### `asset_detail`

```ts
invoke<AssetDetailDto>("asset_detail", { assetId: string })
// { id, rating, triage, favorite, tags: TagDto[], captions: CaptionDto[] }
```

InfoPanel用の一括取得。`captions` は現行のみ (未設定のkindは含めない)。

## 30. Events

### `asset:updated` — メタデータ変更通知

```ts
type AssetUpdatedEvent = {
  ids: string[];              // 変更されたstable AssetId (10進文字列)
  changes: string[];          // "rating" | "cull" | "favorite" | "tags" | "caption"
  rating?: number;            // 変更後の値 (rating変更時のみ)
  triage?: "picked" | "rejected" | "unselected";  // cull変更時のみ
  favorite?: boolean;         // favorite変更時のみ
};
```

- フロントはこのイベントで (1) グリッドセルのキャッシュ済み項目をin-place
  更新 (`CatalogPager.patchItems` — rating/triage/favorite)、(2) フォーカス
  項目なら `asset_detail` 再取得 (tags/caption追従)、(3) セッション集計
  (採用/除外/保留カウント) を再計算する。
- backendも表示済みカタログレコード (`CatalogInner.library`、Library表示中は
  `state.records` も) を同一トランザクション直後に更新するため、以後の
  ページ取得は新しい値を返す (generation不変)。

## 31. タグ正規化規則 (確定)

`normalize_tag_name` (gento-core metadata) — **trim → NFC → ASCII小文字化 →
内部空白の連続を1つの半角空白へ圧縮** の順:

- 前後空白 (全角空白U+3000・タブ含む) を除去
- NFC正規化 (分解形の濁点 `か+゛` → `が` を統合)
- ASCII大文字を小文字へ (`Sky` と `SKY` は同一タグ)
- 内部の空白連続 (種類混在含む) を半角空白1つへ (`青　空` と `青 空` は同一)
- **全角英数字 (ＴＡＧ) はNFKCを適用しないため半角 (TAG) と別タグ**
  (正規化の範囲をASCII小文字化に限定する仕様判断)
- 正規化結果が空 (空白のみ) の入力は拒否 (`Constraint`)
- `tags.normalized_name` はUNIQUE — 表記違いは作成時に既存タグへ統合
  (受入「重複→統合」)。表示名 (`display_name`) は初回登録時の入力 (trim済み) のまま

## 32. ページDTOへの追加フィールド (minor。`GENTO_API_VERSION` 据え置き)

```ts
type ImageListItem = {
  // ... 既存フィールド ...
  rating: number;      // 0..=5 (グリッドの星バッジ)
  favorite: boolean;   // (♥バッジ)
};
```

- library表示は `assets.rating/favorite` から供給。一時走査・仮想は 0/false

## 33. フロントエンド実装 (WP-5。commandではなくWebView側)

- **Undo**: `lib/undo.ts` — メタデータ操作のみのUndoスタック (直近50操作の
  逆適用関数。Ctrl+Z)。before値 (評価/選別/お気に入り/キャプション本文) は
  操作前に捕捉、タグ付与のundoは付与後の名前解決で解除する。
  **ファイル操作のUndoはWP-7で統合** (本WPスコープ外)
- **高速選別モード** (ビューア内トグル): 数字1-5=星評価・0=評価解除・
  X=除外・P/A=採用、入力後自動次送り (`lib/triage-keys.ts` の純粋関数)。
  ON中はズームキー (0=fit/1=actual) より優先。ビューアフォーカス時のみ有効
- **比較表示**: 各スロットに採用/除外ボタン + X=アクティブスロット除外・
  K=アクティブスロット採用 (Pは既存のoriginal昇格と衝突するためK)
- **一括編集**: 複数選択時、InfoPanelの操作は「選択N件へ適用」の確認表示を
  经て実行 (キャプション編集は単一選択時のみ)
- **セッション集計**: 採用/除外/保留カウントは `asset:updated` 駆動で再計算
  (値はDB正本→カタログ項目→セッション集計の流れ)
- **QA-WP4残課題対応**: Minor-6 (同一フォルダ再走査のスクロール復元 —
  空カタログ中のscrollTop=0を保存せず、total復帰時に直前位置へjump) /
  Minor-7 (セッション保存失敗 `lastError` をInfoPanel通知へ表示)
- dev bridge: `M` 行 (編集可否・rating・triage・favorite・タグ数・undo残数・
  高速選別モード) と操作アクション (`metaRate5`・`metaPick`・`metaReject`・
  `metaUndo`・`metaBulkConfirm`・`metaToggleTriage`・`pressKey0/3/5/X/P`) を追加

## 34. Commands (WP-6: コレクション)

コレクションIDは10進文字列 (collections行ID。AssetIdとは別物)。
stable AssetIdを受け取るcommandはWP-5と同じガード (Library表示 + 仮ID拒否)。
すべて**fs無操作** (コレクションは仮想分類。spec §7)。

### `collections_list` / `collection_create` / `collection_rename` / `collection_delete`

```ts
invoke<CollectionDto[]>("collections_list")
invoke<CollectionDto>("collection_create", { name: string })
invoke<CollectionDto>("collection_rename", { collectionId: string, name: string })
invoke<null>("collection_delete", { collectionId: string })
```

- `CollectionDto`: `{ id, uuid, name, kind: "manual"|"smart",
  query_json?: string, item_count: number, created_at, updated_at }`
  (snake_case — WP-5委任仕様系と同じ表記規約)。
- `item_count` は manual=項目数 / smart=**評価結果の現在件数** (list時点)。
- **同名許可** (識別はuuid。WP-6の確定方針)。名前はtrim、空は日本語エラー。
- 削除はitemsごと (CASCADE)。表示中だった場合はbackendが通常ライブラリ
  表示へ戻す (`catalog:changed`)。

### manual項目操作

```ts
invoke<null>("collection_add_items", { collectionId: string, ids: string[] })  // stable AssetId
invoke<null>("collection_remove_item", { collectionId: string, assetId: string })
invoke<null>("collection_reorder", { collectionId: string, assetIds: string[] })  // 全順序送り
invoke<CollectionItemsDto>("collection_items", { collectionId: string, offset?: number, limit?: number })
invoke<CollectionItemDto>("collection_set_item_note", { collectionId: string, assetId: string, note: string | null })
```

- 追加は末尾 (position=追加前の件数)。既存含めは冪等スキップ。
  1トランザクション (不在asset混入でロールバック)。
- **positionは常時0開始連番**: 削除で詰め、reorderで0から振り直し。
  reorderは現在の項目集合と完全一致が必要 (部分更新は`Constraint`エラー)。
- `CollectionItemDto`: `{ asset_id, position, note? }`。
- 同じAssetを複数コレクションへ追加可能 (受入条件2)。

### スマートコレクション

```ts
invoke<CollectionDto>("smart_collection_create", { name: string, queryJson: string })
invoke<CollectionDto>("smart_collection_update", { collectionId: string, name: string, queryJson: string })
invoke<SmartItemsDto>("smart_collection_items", { collectionId: string, offset?: number, limit?: number })
invoke<CollectionDto>("smart_collection_refresh", { collectionId: string })
```

- `queryJson` は§35のschemaを厳密検証 (不正JSON・未知条件型・余分/欠落
  フィールド・値域外は日本語エラー)。保存時は正規形 (has_tag名正規化)。
- `SmartItemsDto`: `{ total, asset_ids: string[] }` (id昇順)。
- manualコレクションへのsmart操作 (update/refresh) はエラー。

### `collection_show` (中央グリッド表示)

```ts
invoke<CatalogSummary>("collection_show", { collectionId: string })
```

- manual=position順 / smart=**表示時評価** のレコードでLibraryモード表示を
  置き換える (generation進行 + `catalog:changed` reason=`collection_switch`)。
- 表示できないasset (presentなFileを持たない) はビューから除外される
  (件数 item_count は保持)。
- コレクション表示中は `set_catalog_sort` が拒否される
  (並び順の正本はコレクション)。ソース切替 (`set_catalog_source`) で
  コレクション表示は終了し通常表示へ復帰する。

## 35. query_json schema (WP-6時点の確定形)

```json
{
  "conditions": [
    { "type": "rating_ge",  "value": 4 },
    { "type": "cull_state", "value": "picked" },
    { "type": "favorite",   "value": true },
    { "type": "has_tag",    "name": "sky" },
    { "type": "mime_type",  "value": "image/png" }
  ]
}
```

- トップレベルは `conditions` 配列のみ (未知キー拒否 — WP-8の検索AST接続時に
  version付きで拡張)。条件は**AND結合**・順序保存。空配列=全Asset。
- `rating_ge`: 整数0..=5 / `cull_state`: "unselected"|"picked"|"rejected" /
  `favorite`: 真偽値 / `has_tag`: タグ名 (保存時に`normalize_tag_name`で正規化) /
  `mime_type`: 非空文字列 (assets.mime_type完全一致)
- **SQL文はユーザーデータとして保存しない** (受入条件4)。条件→SQLは
  gento-db実装が固定断片+パラメータバインドのみで構築する。

## 36. Events (WP-6)

### `collections:changed` — コレクション変更通知 (左ペイン更新用)

```ts
type CollectionsChangedEvent = {
  reason: "created" | "renamed" | "deleted" | "items" | "query" | "refreshed";
  collection_id?: string;
};
```

### `catalog:changed` のreason追加

- `collection_switch`: コレクション表示の開始・切替
- `collection_refresh`: 表示中コレクションの再評価・項目変更反映

## 37. 明示refresh設計 (二重反映防止。WP-6)

`asset:updated` (WP-5) が発生してもスマートコレクションの内容は**自動再評価
しない**。再評価は (1) `smart_collection_refresh` の明示呼び出し、
(2) 次の `collection_show` (表示時評価) のみ。メタデータ編集 → スマート再評価
→ (将来の) 再索引化…というトリガー連鎖を避けるための設計。
編集済み項目のバッジ等は `patch_library_assets` のin-placeパッチでコレクション
ビューにも反映される (membershipの変化のみ明示refresh)。

## 38. フロントエンド実装 (WP-6。commandではなくWebView側)

- `lib/collections.ts`: CollectionsStore (IPCラップ + `collections:changed`購読
  で一覧自動再取得) + 条件ビルダー純粋関数 (`buildQueryJson` /
  `parseQueryJson` / `planReorder` — vitest対象)
- 左ペイン `CollectionsPane.svelte`: manual/smart区別アイコン・件数・作成
  (manual=名前入力 / smart=最小条件ビルダー: 評価≥/選別/お気に入り/タグ/形式
  のドロップダウン+値、AND結合のみ)・改名・削除・再評価
- `CollectionPicker.svelte`: グリッドのコンテキストメニュー「コレクションへ
  追加…」から開くピッカー (既存へ追加 / 新規作成して追加)
- コレクション表示中のツールバー: manual=「選択を先頭/末尾/位置入力へ移動」
  「コレクションから除外」/ smart=「再評価」/ 共通=エクスポート入口
  (無効 + 「WP-14で実装」表示)
- InfoPanel: 表示中manualコレクションのフォーカス項目に「コレクション内メモ」
- 選別セッション: `SessionOriginDetail` に `collection` kindを追加
  (再開時に対象コレクションを再表示してからID解決)
- dev bridge: `CL` 行 (一覧・表示中・メモ・エクスポート入口) とコレクション
  コマンドモード (ctrl+shift+F1 → 数字キーでアクション1回実行。3修飾キー
  ctrl+shift+altはWM/WebKitへ到達しないための代替チャネル)

## 39. Commands (WP-7: 安全なファイル操作とUndo)

### `file_ops_plan` — dry-run (計画)

```ts
invoke<FileOpPlanDto>("file_ops_plan", {
  kind: "copy" | "move" | "rename" | "mkdir" | "trash" | "delete_forever",
  paths: string[],           // 操作対象の絶対パス (登録ルート内)
  destDir: string | null,    // copy/moveの宛先ディレクトリ
  newName: string | null,    // renameの新しい名前
});
```

応答 (`FileOpPlanDto`):

```ts
{
  kind: string,
  steps: Array<{
    file_id: string,          // files.id (10進文字列)
    src: string, dst: string,
    display_name: string,
    conflict: {               // 宛先に同名ファイルが存在する場合のみ
      dst: string,
      dst_byte_size: number,
      dst_mtime_ms: number,
      hash_matches: boolean | null,  // null=未比較 (サイズ不一致等)
      keep_both_name: string,        // "name (2).ext"
    } | null,
    plan_error: string | null,       // src不在・シンボリックリンク等
  }>,
  plan_errors: string[],             // 実行不可 (renameに複数ファイル等)
  dest_root_id: string | null,
  disk_shortfall_bytes: number | null,  // 容量不足 (バイト差)
  confirm_token: string | null,  // FG-WP7: 破壊的操作を含む場合の単発確認トークン
}
```

`confirm_token` (FG-WP7): `kind: "delete_forever"` または上書き解決の
可能性がある衝突を含む計画に対して発行される**単発用トークン**
(乱数nonce・メモリ保持・5分失効・1回限り)。`file_ops_execute` は
`delete_forever` / overwrite解決の実行にトークン一致を**必須**化する
(不一致・失効・再利用はErr)。UIは 計画→確認ダイアログ→トークン付き実行
のフローを組む (チェックボックスのみに依存しないbackend確認ゲート)。

トークンは発行時の**破壊的意図に束ねられている** (FG-WP7-2): planは
`kind + 対象file id (昇順正規化) + 宛先 + 新名 + 上書き対象候補
(計画時の衝突ステップid、昇順)` の正準直列化 (各フィールド長さ前置) の
BLAKE3ダイジェストをトークンと対で保持する。executeは実際の要求から
同じ方式で再計算し、**一致 + トークン一致 + 未使用 + 未失効**を全て
要求する。対象・宛先・kindが異なる要求への流用、およびplan→execute間の
衝突集合変化 (確認ダイアログに一度も表示されなかったファイルの上書き)
は拒否される。いかなる消費試行もトークンを破棄する。

### `file_ops_execute` — 実行 (async command)

```ts
invoke<FileOpOutcomeDto>("file_ops_execute", {
  args: {
    kind: string,
    paths: string[],
    dest_dir: string | null,
    new_name: string | null,
    resolutions: {              // 衝突解決 (未指定の衝突はskip = 安全側)
      global: "overwrite" | "skip" | "keep_both" | null,
      per_file: Record<string, string> | null,  // file_id → 方針
    } | null,
    confirm_token: string | null,  // FG-WP7: 破壊的操作に必須 (plan応答のもの)
  },
});
```

応答 (`FileOpOutcomeDto`): `{ op_id, kind, status: "done"|"partial"|"failed",
undoable, steps: Array<{ file_id, from, to, status, resolution, error,
warning }> }` (`warning` = 完了したが注意喚起がいる事象。例: 跨界移動で
コピー後の元ファイル削除失敗 — 宛先完成済みのため成功扱い)。
**実行は (a) file_operations行をexecutingで確保 → (b) ファイル単位FS+DB
(1tx) → (c) result JSON確定、の順。この応答を受けた時点で操作ログは
確定済み** (安全原則「操作ログ確定後にUI成功通知」)。内部で計画を再計算
する (クライアント由来の計画を信頼しない)。

### `file_ops_undo_check` / `file_ops_undo` — Undo (2段)

```ts
invoke<UndoCheckDto>("file_ops_undo_check", { opId: string });
// → { op_id, ok, kind, refusals: string[] }  前提検証のみ (何も実行しない)

invoke<UndoOutcomeDto>("file_ops_undo", { opId: string });
// → { op_id, fully_undone: boolean, status: "undone"|"partial_undone"|"refused",
//     errors: string[], refusals: string[], rollback_errors: string[] }
```

前提検証 (全アクション): 対象の存在・世代トークン一致
(os_file_id+size+mtime_ns)・戻し先の不在・**ゴミ箱エントリの存在+サイズ一致**
(記録した `TrashEntry` = trash dir + entry名に対する厳密同一性検証。
パス再利用・複数エントリ競合では不一致→拒否)。検証は記録順を考慮した
逐次シミュレーション (上書きステップの複合undo —「新ファイル除去→旧
ファイル復帰」— では除去が復帰先を空けた前提で検証する)。外部変更で
前提が崩れていれば**何も実行せず**理由を返す (安全原則3)。

**Undo原子性ポリシー (FG-WP7): 逐次実行+補償ロールバック**。各アクション
は実行直前に前提を再検証し (FG-WP7-2: 破壊を伴うDeleteCopyもtrash直前に
世代トークンを再照合 — verify通過後の外部差し替えを検知してそのactionを
失敗させる)、失敗・キャンセル時は実行済みアクションを**逆順で補償**する
(各補償も前提検証付き)。全成功でのみ `undone`
(操作行のstatusも `undone`)。それ以外は `partial_undone` +理由
(操作行のstatusは `partial_undone`・undoable=0)。補償にも失敗した項目は
`rollback_errors` へ (手動確認が必要)。Undo対象: copy(生成物削除=ゴミ箱) /
move・rename(巻き戻し) / trash(ゴミ箱復帰) / **上書きステップ(新ファイル
除去+旧ファイル復帰の複合)**。mkdir・delete_foreverは対象外。

### `file_ops_history` / `file_ops_cancel` / `folder_create`

```ts
invoke<FileOpHistoryDto[]>("file_ops_history", { limit: number });  // 既定50・上限200
invoke<void>("file_ops_cancel");  // 協調キャンセル (ファイル境界で停止)
invoke<FileOpOutcomeDto>("folder_create", { parent: string, name: string });
```

## 40. Events (WP-7)

### `file_ops:progress` — ファイル単位の進捗

```ts
type FileOpsProgressEvent = {
  op_id: string;
  index: number;  // 0開始
  total: number;
  from: string;
  to: string | null;
  done: boolean;  // ファイル単位の開始/完了
};
```

## 41. ファイル操作の安全規約 (WP-7。Codexレビュー対象)

- **通常削除はゴミ箱** (`trash`)。`delete_forever` は別コマンドとして分離し、
  **backend確認ゲート** (FG-WP7: plan発行の単発トークン一致がexecuteに必須。
  FG-WP7-2: トークンは破壊的意図の正準ダイジェストに束ねられ、別要求へは
  流用不可。UIは確認ダイアログ+「完全に削除する」チェックボックスの2段明示)。
  ゴミ箱移動の失敗は通常削除へフォールバックしない (エラー表示のみ)。
- **上書きは明示確認 + 旧ファイル保護**: dry-runで衝突を検出し、overwrite
  が明示された場合のみ (1) 旧宛先を先に**ゴミ箱へ退避**し (2) 新ファイルを
  配置する (undoは「新ファイル除去+旧ファイル復帰」の複合)。未指定の衝突は
  skip。「操作先==操作元」の上書きは拒否。上書き実行にも確認トークン必須。
- **最終配置は原子的no-clobber** (FG-WP7): copy/move/keep_bothの最終配置は
  すべて `renameat2(RENAME_NOREPLACE)` (Linux直接syscall。非対応環境は
  link+unlinkフォールバック) で行う。計画→実行間に宛先が出現しても
  **上書きされない** (AlreadyExists失敗 = TOCTOU競合の検出)。
- **ゴミ箱undoの厳密同一性** (FG-WP7): trash実行直後に生成エントリを特定
  (新規trashinfoのPath=一致) し `TrashEntry` (trash dir + entry名) を
  result JSONへ記録。undo/restoreはその正確なエントリを対象に、復帰前に
  存在検証+サイズ検証 (トークンのsize成分) を行う。
- **破壊的undoアクションの実行直前再検査** (FG-WP7-2): copy undo
  (DeleteCopy) はゴミ箱へ移す直前に世代トークン (os_file_id+size+
  mtime_ns) を再照合する。verify通過後にパスが外部差し替えられていた
  場合はそのactionを失敗させ (補償ロールバック発動) 、**異なるファイルを
  ゴミ箱へ送らない**。
- **コピーの中間生成物は非登録**: コピーは厳密形式
  `.{最終名}.gento-tmp-{op_uuid}-{seq}` のテンポラリ名で書き込み・fsync後
  にNOREPLACEで最終名へ確定してからDB反映。走査 (is_image_entry) も名前中
  の `.gento-tmp-` を画像扱いしないため、異常終了残骸も一覧・DBに現れない。
- **一時ファイル掃除の所有権** (FG-WP7): 起動掃除は (a) file_operationsの
  request/result JSONに記録された所有tmpパス、(b) 厳密正規表現一致
  (uuid形式検証含む) かつ登録ルート配下、のみ削除。それ以外の
  `.gento-tmp-` 類似ファイルは残置しログで通知。
- **移動の原子性**: 同一FSはNOREPLACEリネーム。跨界 (EXDEV) は
  copy+fsync+NOREPLACE配置→元削除へフォールバック (元削除失敗は
  warning付き成功 — 宛先完成済みのため)。
- **DB反映**: ファイル単位で1トランザクション。移動=rel_path更新
  (uuid・Asset・os_file_id保持)、ゴミ箱/完全削除/ルート外移動=missing化
  (`.gento-trash` 仮想化はしない — 再出現で復帰)。コピーは元Assetへ即時
  共有リンク (hash共有)。
- **watcher抑制 + 和解スキャン** (FG-WP7): 操作実行中、対象パス (元・先)
  の監視イベントを破棄 (WatchBuffer抑制キー) するが、**破棄したパスを記録**。
  操作完了後 (`operation_finished`) に影響ディレクトリのscoped差分スキャン
  (監視イベント適用と同じ差分エンジン) を発火し、抑制中の外部変更を見逃さ
  ない。
- **異常終了回復**: 起動時に `executing` 残留のfile_operationsを検出→
  `failed`確定+再スキャンで収束 (「安全な再開」= 失敗項目のUI再試行)。
- IDs: file_id/op_idは10進文字列 (JS安全整数規約に合わせる)。
- `GENTO_API_VERSION` 据え置き (1): 追加のみで破壊的変更なし。
