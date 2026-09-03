# GEN-TO IPC API契約 (Phase 2 / WP-2時点)

- 作成日: 2026-08-28 (WP-2)
- 最終更新: 2026-09-03 (**FG-WP13-2: §47 改訂 — rel_path symlink規約を全コンポーネント歩行へ強化・列挙fail-closed・durability境界明記** / FG-WP13-1: §47 改訂 — トークン束縛された確定plan適用・rel_pathパス検証規約・digest manifest完全性・フルDB原子公開 / WP-13 (MVP): §47 バックアップと復元のcommand群・`.gento`バックアップ形式追加 / 2026-09-02 WP-12 §46 / WP-11 §45 / WP-10 §44 / WP-9 §43 / WP-8 §42 / WP-7 §39-41 / WP-6 §34-38 / WP-5 §31-33 / WP-4 §26-33 / WP-3 §22-24 / WP-2 §17-21 / WP-1 §1/§16 / WP-0 §0/§1/§14/§15)
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
  deferred: number;        // キュー満杯でbacklogへ繰延 (generate_allのみ。
                           // 完了駆動で空きができ次第投入される。WP-16 F1)
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
invoke<{ cancelled_queued, cancel_requested, cancelled_backlog, not_found,
         skipped_unreadable }>(
  "cancel_thumbnail_requests", { ids: string[], sizeClass })
```

- 未開始=即時除去 (イベントは出ない)。実行中=協調キャンセル要求
  (ジョブがデコード前/リサイズ前/エンコード前で検査)。
- `cancelled_backlog` (FG-WP16-3 B2 / FG-WP16-4 R1): プールに**未投入**の
  generate-all繰延分 (backlog) から破棄した件数。対象はdeque内のentryと
  **feederがpop直後・まだプールへ投入していないitem (inflight) の両方**。
  feederのpop→投入はbacklogロックで守られた2区間 (pop時にinflight登録 →
  cancel消費/世代検査/プール投入を同一ロック内で完結) で行われ、cancelも
  同じロック内で (a) deque内の一致key破棄 (b) inflightの一致key破棄を
  行うため、**cancelが`not_found`を報告したキーがその後feeder経由で
  プールへ投入される経路は構造的に存在しない** (破棄済みキーは
  `not_found` に二重計上しない)。

### `generate_all_thumbnails`

```ts
invoke<RequestAccepted>("generate_all_thumbnails", { sizeClass: SizeClassId })
```

- 表示中カタログ全体を `background` 優先度で一括登録 (受け入れ条件2/5の検証用)。
- キュー上限 (4096) を超えた分は**破棄されずbacklogへ繰延**され
  (`deferred` に件数)、プール完了駆動で空きができ次第投入される
  (WP-16 F1。ライブラリのハッシュ再提出と同一の需要駆動方式)。
  排液完了・再要求による置換・終了/キャッシュ全削除での残数はログで報告。
- **置換の世代規約 (FG-WP16-3 B3 / FG-WP16-4 R2)**: 各 `generate_all` 呼び出しは単調
  増加のgeneration tokenを取得する。呼び出し開始時にbacklogは即座に
  新世代へ置換され、繰延分は**単一ロック+世代検査で一括install**される
  (より新しい呼び出しが開始済みなら破棄)。このためbacklogは常に
  **最新の要求の資産のみ**を含む (並行押下・排液中の再押下で混入しない)。
  feederもpop済みitemの世代をプール投入**前**にbacklogロック内で検査する
  ため、**より新しい `generate_all` 開始後に旧世代itemがプールへ投入される
  経路は存在しない**。ただし**プールへ既に投入済み**の旧世代jobは取り消され
  ず、その完了は**冪等なキャッシュ書き込みのため許容**する (内容はキーに
  対応する正当なサムネイル。cancelで明示的に止めることは可能)。
  満杯でbacklogへ戻る場合も同一ロック内の世代検査で破棄される
  (再投入されない)。

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
- **起動時の既定表示** (UXLIB-1。WebView側 `startupCatalogView` の判定):
  health の `dataset_path` が登録ルート配下、または dataset 不在
  (`dataset_exists: false`) かつ登録ルートありの場合は `library` 表示で
  起動する (登録済み画像が見えない・編集できないadhoc表示と重複走査の
  解消)。存在する未登録 dataset は従来どおり `temp` 一時走査 (計測の
  決定論化を維持)。UIからの切替導線: 登録ルート行/「ライブラリを表示」
  ボタン (LibraryPanel)・`addRoot` 成功後の自動切替・ツリーの登録済み
  フォルダ選択が同じ `library` ソースへ遷移する。

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
  (起動時の走査対象は `--dataset` が正本で決定論的。ただしUXLIB-1以降、
  dataset が登録ルート配下・または dataset 不在で登録ルートありの場合は
  起動直後にライブラリ表示へ切替する (§17「起動時の既定表示」)。
  前回フォルダはツリーの選択ハイライトとして復元)
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

## 42. WP-8: キーワード検索・FTS5・フィルター

### FTS5スキーマ (migration #7)

```sql
CREATE VIRTUAL TABLE asset_docs USING fts5(
    asset_id UNINDEXED,
    filename,
    path,
    tags,
    captions,
    note,
    tokenize='trigram'
);
```

- **通常のFTS5テーブル** (contentlessではない): 文書本文を保持するため
  `snippet()` が使え、LIKEフォールバックも同一テーブルへ投げられる
- **rowid = asset_id の1:1対応**で管理する (`asset_id` はFTS列でないため
  `WHERE asset_id` は全文走査になる。rowid経由のDELETE/INSERTで索引更新が
  O(log n) — 5万件全再構築が2〜3秒に収まる根拠)
- **検索文書の構成** (data-model §9): ファイル名 (全present File・重複
  除外) / パス (同・lossy表示) / タグ (正規化名) / キャプション
  (description + sd_tags の現行行) / user_note。
  **embedded_prompt はWP-9で接続予定 (現未対応)**。negative prompt・
  workflow全体は意味文書に含めない (明示検索もWP-8時点では不可)
- **更新方針**: トリガー不使用。タグ/キャプション/ファイル操作のrepo書き込み
  と**同一トランザクション**で対象assetの文書をDELETE+INSERT再構築
  (Rust駆動の正本)。リネーム・移動・リンク・不在化も同一経路
- **初期索引**: 走査後 `library_snapshot_changed` のタイミングで件数照合
  (軽量) → 不整合時のみ背景修復 (`fts_repair`)。走査中の文書構築は
  link_file等のフックで進行中に完了する。本格進捗UIはWP-10ジョブ基盤で本化

### 検索構文 (gento-search parser)

```text
青い髪の少女 tag:night rating:>=3 type:png
model:"model-name" steps:>=30 has:workflow
added:2026-01-01..2026-06-30 path:"vacation/2026" fav:true cull:picked
```

- 空白区切りのterm群 (全AND)。**空クエリ = 全件**
- フリーテキスト (裸語 / `"引用句"`) は全文一致。**trigram仕様のため
  3文字未満トークンを含む語句は検索文書テーブルへのLIKEフォールバック**
  で一致させる (大文字小文字はASCII範囲で無視)
- `key:値` = 等価。`key:>=値` `<=` `>` `<` = 比較 (ratingのみ意味を持つ)。
  `key:"句"` = 引用値 (空白・記号入り)
- keys: `tag` (正規化一致) / `rating` (0..=5) / `cull`
  (unselected|picked|rejected。採用/除外/未選別も可) / `fav`
  (true|false) / `type` (拡張子 png等 または完全MIME) / `path` / `name` /
  `note` (部分一致) / `added` (`YYYY-MM-DD..YYYY-MM-DD` 片端可・単日可)
- **未対応キー** `model` / `steps` / `has` は**エラーではなく警告**を返し
  無視される (WP-9で画像メタデータ接続時に有効化)
- パース失敗は日本語エラー + 文字位置 (`位置: N文字目`)
- **GUIフィルタはAST term JSONを組み立てて投入**し、クエリ文字列とAND合成
  される (`search_run` の `filters` 引数。形状は下記)

### AST term JSON (filters / 保存形式共通)

gento-search `SearchQuery` の直列化と同一形状:

```json
{"type":"text","value":"夜景"}
{"type":"field","key":"rating","op":"ge","value":{"number":3}}
{"type":"field","key":"cull","op":"eq","value":{"text":"picked"}}
{"type":"field","key":"fav","op":"eq","value":{"bool":true}}
{"type":"field","key":"tag","op":"eq","value":{"text":"night"}}
{"type":"field","key":"added","op":"eq","value":{"date_range":{"from":{"year":2026,"month":1,"day":1},"to":null}}}
```

### Commands

- `search_run(query, filters?, scopeRootId?, scopeCollectionId?)` (async) →
  `{summary, total, shown, elapsedMs, warnings, queryJson}`。
  検索を実行し結果を**カタログの検索ソース**として表示する
  (mode=Library系・安定AssetId。`catalog:changed` reason=`search_switch`)。
  全件収集はspawn_blocking (UI非ブロッキング)。`set_catalog_source("search")`
  で最後の検索結果へ再切替できる
- `search_refresh()` (async) → 表示中検索を新しいDB状態で再実行
  (メタデータ編集後の反映用。同じ応答形状)
- `search_evidence(assetIds: string[])` → `[{assetId, matches:
  [{field, snippet}]}]`。一致フィールド (filename|path|tags|captions|note)
  と `[一致語]` 形式の前後文。InfoPanel・セルツールチップが使用
- `fts_integrity()` (async) → `{expectedDocs, actualDocs, missingDocs, orphanDocs,
  duplicateDocs, staleContents, consistent}`
- `fts_rebuild()` (async) → `{rebuilt, removed, elapsedMs}`。
  進捗は `fts:rebuild_progress` イベント `{done, total}`
- `fts_repair()` (async) → 不整合のみ修復 (欠落・内容不一致の再構築 + 孤立・重複削除)

### スマートコレクション = 保存済み検索 (設計判断)

- **「保存済み検索」と「スマートコレクション」は同一実体の別ビュー**
  (二重実体を作らない)。左ペインのsmartコレクション表示ラベルを
  「保存済み検索」としている (実体はcollections.kind='smart')
- query_jsonは **version 2 (AST正本)** で書き込む:
  `{"version":2,"query":{"terms":[...]}}`。
  `smart_collection_create` は `searchJson` 引数 (version 2形式) を取り、
  指定時はそちらを正本とする (旧`queryJson`引数は互換用)
- 旧形式 (WP-6最小条件 `{"conditions":[...]}`) は**読み取り互換**:
  評価時にASTへ変換 (`legacy_to_query`)。UIからの新規書き込みは
  version 2のみ (QA-WP8よりCollectionsPaneの条件ビルダーもv2を書く。
  旧`queryJson`引数での旧形式作成は既存呼び出し側 (E2E等) の互換用に
  受け付ける — `searchJson`指定が優先)
- **スマートコレクション評価はWP-6のSQL直クエリからAST検索経路へ移行**
  (`smart_collection_refresh` / `collection_show` / 件数表示はすべて
  `search_asset_ids` で評価。WP-6互換のためpresentファイル制約は掛けない)
- 検索UIの「保存」ボタン → `smart_collection_create(searchJson=<AST>)`

### AST→SQL の安全規約

条件→SQLは固定断片 + パラメータバインドのみ (WP-6 smart_whereと同じ規約。
FTS MATCH式はgento-searchが構築・引用符エスケープする)。LIKEは
`ESCAPE '\'` + `%`/`_`/`\` エスケープ。

### 5万件性能 (合成行計測 — cargo test search_performance_50k_synthetic)

release測定 (作業マシン): 50,000 asset + 文書 (タグ5,000件付き) で
p50=27ms / **p95=78.5ms** / max=78.9ms (シナリオ×10: 日本語LIKE高頻度・
狭束縛・構造化・複合・日付範囲)。全再構築 2.3秒。詳細は
docs/dev/measurements/ とhandoff参照。

### 制約・保証

- 検索結果はpresentなFileを持つassetに限定 (グリッド表示可能集合と同一)
- 検索実行はreader接続かつUIスレッド外: `search_run` / `search_refresh` /
  `fts_integrity` / `fts_repair` はasync commandで、全件収集・索引保守を
  `tauri::async_runtime::spawn_blocking` で実行する (QA-WP8。5万件級の
  広域クエリでもUIをブロックしない。結果到着後に表示)
- 検索結果表示中はlibraryスナップショットの自動反映をしない (コレクション
  表示と同じ方針。「再実行」で明示更新。`asset:updated` のin-placeパッチは
  適用される)
- **フロントエンドのカタログソース状態も検索表示中はlibrary系扱い**
  (`runSearch`/`refreshSearch`/`exitSearch` がlibraryへ遷移 — backendの
  `CatalogMode::Library` と一致。showCollectionと同じ規則。一時走査
  (temp) ビューから検索しても、検索結果は安定AssetIdのためInfoPanel編集・
  asset_detail取得が有効。メタデータ編集可否の規則正本は
  `lib/metadata.ts canEditMetadata`)
- `GENTO_API_VERSION` 据え置き (1): 追加のみ

## 43. WP-9: 埋め込み画像メタデータ (EXIF / IPTC / PNG生成メタデータ)

### 基本方針 (spec-draft §12)

- **読み取り専用**: 元画像のバイト列は一切変更しない (BLAKE3前後一致を
  cargoテスト `metadata_extraction_does_not_modify_file_bytes` で保証)
- 2層構造: 正規化列 (`asset_metadata` 既存列) + 生データ
  (`raw_exif` / `raw_iptc` / `raw_png_metadata` / `raw_workflow` のJSON)
- 未知メタデータも破棄せずRaw表示・コピー可能 (未知チャンクはpng-text行の
  raw JSONへ。上限超過チャンクは `__warnings` 配列でスキップ記録)
- **migrationなし**: 必要列はすべてmigration #1に存在
  (`UNIQUE(asset_id, parser_name)` で1 asset = 複数parser行の縦持ち)

### parser一覧 (parser_name@version)

| parser | 対象 | 主な正規化先 |
|---|---|---|
| `exif@1` | JPEG APP1 / TIFF / PNG eXIf / WebP EXIF (kamadak-exif) | taken_at (RFC3339), camera_make/model, lens, gps_lat/lon (十進度・S/W負), orientation + `raw_exif` (全フィールドJSON) |
| `iptc@1` | JPEG APP13 8BIM内IIM (最小自作) | 正規化列なし。keywords/caption/byline/copyright/title等を `raw_iptc` JSONへ (未知datasetは `r:d` 生キー) |
| `png-text@1` | tEXt / iTXt / zTXt 全収集 (自前走査) | 正規化列なし。全チャンクを `raw_png_metadata` JSONへ |
| `sd-params@1` | `parameters` チャンク (SD WebUI/Forge形式) | generator="Stable Diffusion WebUI", positive/negative_prompt, steps, cfg_scale, seed(文字列), sampler, scheduler(Schedule type), generation_model(Model), vae + `generation_extra` JSON (size/model_hash/clip_skip/loras等) |
| `comfy@1` | `prompt` チャンク (API JSON) + `workflow` チャンク | generator="ComfyUI"。KSampler/KSamplerAdvancedのpositive/negative入力を接続追跡 (CLIPTextEncode・ConditioningCombine等、ベストエフォート)。steps/cfg/seed/sampler/scheduler。CheckpointLoader→generation_model, VAELoader→vae, LoraLoader→loras。`workflow` チャンクは `raw_workflow` へverbatim (解析しない) |
| `nai@1` | `Comment` (JSON) + Title/Description/Software/Source | generator="NovelAI"。Description(またはComment.prompt)→positive, Comment.uc→negative, steps/scale/seed/sampler, Source→generation_model |

- 読み取り上限: PNGテキストチャンク **1チャンク4MiB / 合計16MiB**
  (超過は警告付きスキップ)、EXIFコンテナ抽出 (TIFF/WebP) の読み取り総量
  **64MiB**、チャンク/セグメント/フィールド数上限 (異常ファイルへの安全弁)
- Seedは表現差とu64超過のため**文字列**保持 (data-model §6)

### 世代トークン規律 (WP-2パターン踏襲)

- 解析はハッシュと**同一fd**で束ねる: `ScanFs::hash_and_probe_and_metadata`
  (gento-image `hash_probe_metadata_file`)。fdトークン照合→BLAKE3→
  プローブ→メタデータ抽出→fdトークン再照合。Stale時は解析結果ごと破棄
- 保存はリンクと同一書き込みクロージャ内 (`hash_and_link_file`):
  DB行の現在トークンと突合後に `save_asset_metadata`。同version行の
  再保存は冪等スキップ (内容はcontent hashから決定的)
- **positive_promptは `embedded_prompt` キャプション** (kind=
  embedded_prompt, source=embedded_metadata) としても保存し、FTS文書の
  captions列へ接続 (WP-8の申し送り実装)。手動編集済み現行行は上書きしない

### FTS文書構成の変更 (§42からの差分)

- captions列: description + sd_tags + **embedded_prompt (positive)**。
  **negative prompt・workflow全体は含めない** (data-model §9)
- 検索ASTキー有効化 (旧FUTURE_KEYSの警告を削除し正式キーへ):
  - `model:"…"` / `sampler:…` / `vae:…` / `lora:"…"` — asset_metadata列
    (generation_model/sampler/vae) またはgeneration_extra JSONのloras配列
    への部分一致 (EXISTS。NeはGUIフィルタのASTのみ)
  - `steps:>=30` 等の数値比較
  - `has:workflow` — raw_workflow IS NOT NULL (Ne = workflow非保持)

### Commands

- `asset_metadata_get(assetId)` (async) → `{id, parsers:
  [{parserName, parserVersion, rawExif?, rawIptc?, rawPng?, rawWorkflow?,
  updatedAt}], normalized: {takenAt, cameraMake, …, positivePrompt,
  negativePrompt, generationExtra}}`。normalizedは複数parser行のマージ
  (優先順位: 写真系=exif、生成系= sd-params > comfy > nai)。rawを含む
  全行読み取りのためspawn_blocking (QA-WP8方式)
- `metadata_reparse()` (async) → `{targeted, reparsed, changed, skipped,
  elapsedMs}`。現行parser集合 (`gento_image::metadata::CURRENT_PARSERS`) に
  対し (a) 行が1つもない (b) 既存行のversionが古い (c) 廃止parser名の行
  を持つ asset を代表presentファイルのfdからトークン照合付きで再解析
  (1回最大500 asset。バッチ実行)。進捗は `metadata:reparse_progress`
  イベント `{done, total}`。Stale (ファイル変化) はスキップ扱い
  (次回スキャンで再ハッシュされる)。**parser集合の変更 (追加・削除) は
  全parser version繰り上げの運用** — 形式ごとの行構成差を「行の欠落」で
  判定できないため

### フロントエンド (InfoPanel)

- 「写真メタデータ」節: 撮影日時・カメラ・レンズ・GPS座標 (十進度+方位)・
  向き。GPSのAI送信設定は登録ルート単位である旨を注記 (設定接続はWP-10)
- 「生成メタデータ」節: ツール/モデル/VAE/Sampler/Steps/CFG/Seed +
  **positive/negative promptは折りたたみ** (展開時にのみレンダリング。
  プレビューは先頭4096文字、コピーは全文) + **workflow JSON折りたたみ+
  コピー** (raw_workflowをverbatim — ComfyUIへそのまま読み込ませ可能)
- 「Raw metadata」節: parser行ごとの折りたたみ+コピー
- ロジック正本は `lib/asset-metadata.ts` (純関数。vitestで検証)
- dev bridge (E2E): `X` 行 `photo= gen= raw= pos=<文字数> neg= wf=<文字数>
  wfx=<展開> rawn=<Raw折りたたみ数> cpy=<最後のコピー>`。アクションは
  foCmdMode (`ctrl+shift+F12`) の `p`=折りたたみ全展開 / `i`=workflowコピー

### 既知の制限

- A1111のJPEG出力 (EXIF UserCommentへのparameters格納) は非対応
  (PNGテキストチャンクのみ)
- AVIFのEXIF (メタデータボックス内) は未対応 (EXIF対象はJPEG/TIFF/PNG/WebP)
- GIF/BMPは埋め込みメタデータ対象外 (該当parserなし)
- ComfyUIの条件ノード解決は既知クラスのみ (未知ノードはunknown扱い=
  プロンプト抽出なし。rawのprompt JSONで確認可能)
- 1ファイルに複数生成形式が混在する敵対的ケースは正規化優先順位
  (sd-params > comfy > nai) で決定論化

## 44. WP-10: 永続ジョブ基盤とOpenAI互換AI

### migration #8 (`wp10_ai_profiles_runs_jobs`)

- `ai_profiles` (data-model §8.1 + `enabled`): 接続プロファイル。
  **`api_key_ref` は資格情報ストア参照のみ** (`"keyring:<uuid>"` /
  `"file:<uuid>"`)。鍵本体をDBへ書く経路は存在しない
  (検証: gento-ai `e2e_api_key_sent_as_bearer_and_never_in_db` —
  DB/WAL全バイト走査)。
- `ai_runs` (§8.2): 試行ごとの実行履歴 (status: running/succeeded/failed。
  再試行のたびに新規行 — UPDATEで使い回さない)。`profile_id` は
  ON DELETE SET NULL (プロファイル削除で履歴は失われない)。
- `asset_tags.ai_run_id` / `captions.ai_run_id` をADD COLUMNで追加
  (#1計画どおり。NULL既定のADD COLUMNならFK参照を付与可能)。
- `jobs` 表再構築: status CHECKへ `retry_waiting` を追加。バックオフ期限は
  既存 `scheduled_at` を流用。pending中の同一 (job_type, target_id) 重複を
  防ぐ部分UNIQUE index `idx_jobs_pending_dedupe`。コピー時の正規化:
  実行器の存在しないjob_typeの行とpending重複の2行目以降は除外
  (v7以前のjobs表にCHECKが無かったことへの移行防衛 — populated v7
  移行テストで検証)。

### ジョブ状態機械 (spec §11.2)

```text
queued ──claim──▶ running ──成功──▶ succeeded
  ▲                 │失敗・retryable (attempts<max)──▶ retry_waiting (scheduled_at=期限)
  │                 │失敗・permanent (4xx/認証/パース/応答過大/鍵取得失敗)──▶ failed
  │                 │失敗・retryable (attempts>=max)──▶ failed ──retry_failed(手動)──▶ queued
  │                 └(起動回復: running→queued)
  └──cancel──▶ cancelled (queued/retry_waitingのみ。runningは中断不可)
```

- バックオフ: `gento_core::jobs::backoff_delay_ms` (純粋関数)。
  基準 30s→2m→8m (×4) + ジッター [0.5, 1.5)倍 + 上限1h。
- エラー分類: network/5xx/timeout = **retryable** (backoffで再試行)、
  4xx/認証/パース失敗/プロンプト過大/応答過大/鍵取得失敗 =
  **permanent** (`fail_job_permanent` で即座にterminal failed。
  retry_waitingに入れない — 画像再開示の反復を構造的に防止)。
- worker: gento-tauri `ai::AiService` (std::thread ×2。library.rsの
  ワーカー規約に同じ)。起床 = エンキュー/resume通知 or 500msポーリング。
  プロファイル毎のconcurrencyは「容量チェック・claim・スロット予約を
  同一ロック内」で強制 (チェック〜予約間の競合を構造的に排除)。
- キュー一時停止はメモリフラグ (永続化しない — 再起動で再開が既定)。
  claim直前にも再検査する (pause境界はbest-effort)。

### HTTPクライアント境界 (FG-WP10)

- 応答ボディは2 MiB上限 (Content-Length検査 + ストリーミング読み取り
  上限の二段構え。chunked対応)。超過は非リトライの `AiError::Oversized`。
- リダイレクトは不追従 (`redirect::Policy::none()`) — 3xxはそのまま
  APIエラー (permanen失敗) として扱う。
- プロファイルに鍵参照があるが取得に失敗した場合 (keyring/fileエラー) は
  **フェイルクローズ**: ジョブをpermanent失敗 (「資格情報の取得に失敗」)
  とし、未認証での送信は行わない (4種テスト機能も同じ)。

### 送信プライバシー (spec §16)

- EXIF/GPS送信 = `library_roots.send_exif/send_gps` ∧
  プロファイル追加設定 (`extra_json` の `send_exif/send_gps`)。
  GPSはsend_exifとは**独立した**opt-in (EXIF許可でもGPS拒否なら座標は
  送らない。逆にEXIF拒否でもGPS許可なら座標のみ送る)。
  実効判定は `gento_ai::prompt::effective_privacy`。
- 送信画像は必ず再エンコード (長辺 `max_image_edge` 既定1024px・JPEG q85
  ・EXIF等メタデータは再エンコードで除去)。
  `max_image_edge` は**64〜2048に強制** (3層: IPC保存は範囲外を拒否 /
  保存済み値の読み込みはクランプ / 実行時もクランプ)。旧「0=縮小なし」
  は廃止 — 原寸が外部へ出る経路は存在しない。
- プロンプト: `PROMPT_VERSION = 1` (ai_runs.prompt_versionに記録。
  変更時は必ず版を上げる — 自動処理の重複抑止が版ベース)。

### APIキー保管

1. keyring crate (Linux=Secret Service via zbus / macOS=Keychain /
   Windows=Credential Manager) を試行
2. 利用不能 (ヘッドレス等) の場合、`{data_dir}/ai-secrets/<uuid>.key`
   (0600・平文・README警告同梱) へフォールバック。ディレクトリは0700。
   **symlink (鍵ファイル・ディレクトリ両方) は保存も読み出しも拒否**
   (差し替え攻撃の防衛。フェイルクローズ)。
   簡易難読化は意図的にしない (同一マシンに鍵を置く疑似安全を避ける)。
3. `ApiKey` newtypeのDebug/Displayは `****` (ログ混入の構造的防止)。
4. 鍵削除は「DB参照クリア → 成功後secret削除」の順 (DB失敗時は
   credentialが失われない)。

### Commands (camelCase引数)

- `ai_profile_list()` → `[{id, uuid, displayName, baseUrl, model,
  visionCapable, enabled, timeoutMs, concurrency, extra, hasApiKey,
  apiKeyStore}]` (鍵本体は返さない)
- `ai_profile_save(profile, apiKey?)` — apiKey: 省略=変更なし / `""`=削除 /
  値=保存。`extra` はProfileExtra (省略時は既存値引継ぎ)。
  `max_image_edge` は範囲外 (64未満・2048超) を日本語エラーで拒否
- `ai_profile_delete(profileId)` — 待機中ジョブをキャンセルし鍵も削除
- `ai_profile_test_connection|test_text|test_vision|test_structured(profileId)`
  → `{ok, detail, elapsedMs, status}` (保存済みプロファイルに対して実行)
- `ai_jobs_enqueue(assetIds, force?)` — 手動投入。force無効時は
  「同prompt_versionで成功済み」assetをスキップ (§11.4 既存結果の保持)
- `ai_root_pending_count(rootId)` — 未処理件数 (確認ダイアログ用)
- `ai_jobs_enqueue_all_for_root(rootId, confirmed)` — confirmed=falseは
  エラー (§11.3「件数を提示してから開始」のbackend側ゲート)
- `ai_jobs_list(status?, limit?)` → `{jobs, counts, paused}`
- `ai_job_cancel(jobId)` — queued/retry_waitingのみ
- `ai_jobs_pause()` / `ai_jobs_resume()` / `ai_jobs_retry_failed(jobIds?)`
- `ai_run_history(assetId)` → `[{id, profileId, operation, status,
  promptVersion, requestSummary, error, startedAt, finishedAt}]`
- `ai_root_settings_set(rootId, aiEnabled, sendExif, sendGps)` —
  ルート設定 (watch_enabledは既存値保持)
- `library_roots_list` のDTOに `ai_enabled/send_exif/send_gps` を追加

### Events

- `ai:jobs` — `{changes: [AiJobDto], counts: {status: n}, paused}`。
  ワーカーの状態変化を500msバッチで送出 (emitterスレッド)。
  フロントはInfoPanelのAI履歴・ジョブ一覧の更新に使用。

### 自動処理 (spec §11.3)

- ハッシュ確定 (HashJob成功) ごとに `ai::on_asset_linked`:
  ルートai_enabled=1 ∧ 有効プロファイル存在 ∧ 同prompt_versionで未成功
  の場合のみ投入。**初回登録時の全件処理は自動では行わない** — UIが
  件数を提示し、ユーザー確認のうえ `ai_jobs_enqueue_all_for_root
  (confirmed=true)` を呼ぶ。
- AI endpoint停止中: ジョブは接続失敗 (retryable) → backoffで再試行
  (上限3試行でfailed)。UI・ファイラー機能は同一DBのreader/writer分離と
  専用スレッドで阻害されない (検証:
  `service_e2e_endpoint_down_backoff_and_filer_unblocked`)。

### 手動編集保護 (spec §11.4。acceptance 6)

- タグ: AI付与は `INSERT ... ON CONFLICT DO NOTHING`。既存付与
  (手動・AI問わず) は一切更新しない。
- キャプション: 現行行が `manually_edited=1` なら現行を保持し、AI結果は
  非現行 (current=0) のai行として履歴へ保存。それ以外は旧現行を降格して
  新現行ai行を挿入 (旧AI行は履歴に残る)。

### 既知の制限 / 実endpoint確認項目

- **実llama.cpp/OpenAI等の実endpointへの接続は手動確認項目**
  (本リポジトリの自動証明は in-process mock OpenAI互換サーバー
  `gento_ai::testserver` 相手 — Bearer検証・遅延・500注入・
  response_format非対応の模倣を含む)。
- `response_format: json_object` は既定で送らない (対応確認は
  test_structured)。プロファイル `extra.send_response_format=true` で明示ON。
- ジョブ実行中 (running) のキャンセルは不可 (HTTP呼び出しの中断なし)。
- プロンプト本文はタグ+キャプション一括要求 (operation=
  caption_and_tags)。tagsのみ/captionのみの分割は将来拡張。

## 45. WP-11: 自然言語検索 (ローカルruri-v3埋め込み)

### migration #9 (`wp11_embedding_jobs_app_settings`)

- `jobs` 表再構築: job_type CHECKへ `embedding` を追加 (データは恒等コピー)。
- `app_settings` (key TEXT PRIMARY KEY, value TEXT): MVP設定kv。
  現在使用するキー: `embedding.enabled` ("0"/"1")・`embedding.model`
  ("ruri-v3-70m" / "ruri-v3-30m")。WP-15で本格設定schemaへ整理予定。

### モデル管理 (docs/embedding-runtime.md §3/§4が設計正本)

- models_dir解決: `--models-dir`引数 → `GENTO_MODELS_DIR`環境変数 →
  `AppDirs.data_dir/models`。`.local-models/` (gitignored) を指定して
  開発・E2Eで使う。
- manifest正本 = `docs/dev/ruri3-onnx-manifest.json` を
  `include_str!`で埋め込み (drift不可能)。sha256+サイズ検証をパスした
  パッケージのみロード可能。検証失敗・不在でもアプリは継続
  (acceptance — statusの`packages[].state`で観測)。
- HFダウンロード: `gento_embedding::download` (進捗コールバック・
  キャンセル・Range再開・空き容量確認・staging→検証→atomic rename。
  プロキシ環境変数尊重 — gento-aiと分離したClient)。**MVP既定では
  配布repo未設定 = 未使用** (コマンドからの呼び出し経路なし)。

### 独立ワーカー (`gento-embedding-worker`バイナリ)

- 通信: stdio長さ前置JSON (4バイトLE u32 + JSON。64MiB上限)。
  コマンド: `load` / `unload` / `embed_query` / `embed_documents` /
  `status` / `shutdown`。画像データは流さない。
- 明示load/unload・起動時自動ロードなし・アイドル10分アンロード
  (ワーカー側)・intra-thread既定 = 論理コア/4 (≈物理コアの半分。
  `GENTO_EMBEDDING_THREADS`で上書き)。
- ホスト側 (gento-tauri `embedding::EmbeddingService`): lazy spawn・
  死亡検出はreaderスレッドEOF・再起動バックオフ 1s→4s→…(cap 60s)・
  120秒窓内3回死亡で自動再起停止+`embedding:notice`イベント+
  `worker.phase="crashed"` (enableで手動復帰)。

### 埋め込みジョブ

- jobs `embedding` (target_type="asset"。payloadに`source_hash`)。
  優先度: 初回掃討=6 / 差分 (走査・メタデータ変更)=4。
  クエリ埋め込みはキューを経由しない (直接`embed_query` = 最優先)。
- 差分検出: 検索文書 (gento-core `compose_search_document` — caption
  512字/prompt 512字/タグ30個/note 256字/ファイル名512字の決定的構成。
  negative prompt・workflow・パスは除外 = FTS文書と同じ意味論) の
  BLAKE3 hash を `embeddings.source_hash` と比較。変化 asset を掃討
  (30秒毎・有効時) でenqueue。
- ベクトル保存: `embeddings`表 (purpose='doc'・dtype='f32'・vector BLOB =
  little-endian f32列。UNIQUE(asset_id, purpose, model_name, model_revision)
  でupsert)。USearch索引はWP-12。

### Commands (すべてasync + spawn_blocking。DTOはcamelCase)

- `embedding_status` → `EmbeddingStatusDto`: enabled・model・modelsDir・
  packages[] (modelId/state/detail/packageDir/embeddingDim/license)・
  worker (phase/restartCount/lastError)・loadedModel・embeddedCount・
  totalCount・pendingJobs・selfcheckMargin・warning。
  state: "verified"|"missing"|"missing_files"|"mismatch"。
  worker.phase: "not_started"|"running"|"restarting"|"crashed"。
- `embedding_enable(model?)` → 検証失敗時Err (有効化しない)。
- `embedding_disable` / `embedding_model_delete(model)`
  (ベクトル+パッケージ削除。通常検索への影響なし)。
- `embedding_similar_debug(assetId? | queryText?, k?)` →
  `[{assetId, score}]` — 総当たりcosine (WP-12のUSearch前の検証用。
  **UI非公開**)。

### Events

- `embedding:notice` (payload: string): ワーカークラッシュループ停止・
  ジョブ実行エラー等の通知。

### セルフチェック (acceptance 5-4)

- ロード完了後に固定対 (近い/遠い文書) のcosineマージンを計算し
  `selfcheckMargin`へ公開。0.05未満で`warning`設定 (UI表示)。

### 実測 (docs/dev/embedding-eval.mdに詳細)

- Recall@3 = 0.8333 (閾値0.80。公式ST同一接頭辞と完全一致)。
- 参照実装とのベクトル一致 cos ≥ 0.9999993。
- embed_query avg 9.6ms・docバッチ 11.5ms/件 (release・70m)。

## 46. WP-12: USearch索引・ハイブリッド検索・類似画像検索

### USearch索引 (gento-embedding `vector_index`)

- **依存**: `usearch` =2.26.2 (Apache-2.0。cxx-buildによるC++17その場
  コンパイル + numkong静的リンク — dependency-notes参照)。
- **索引単位・保存先**: モデルidentity (model_name + model_revision +
  dimensions) ごと。`{data_dir}/indexes/{model-id}/` 配下に
  `index.usearch` (HNSW本体) と `identity.json` (sidecar = identity +
  count + version) を保存。**sidecarと索引ヘッダの両方でidentityを検証**
  — 不一致は「混在」エラーとして拒否 (§10: 異なるモデル/revision/次元の
  混在禁止)。
- **metric**: Cos (ベクトルはL2正規化済み → similarity = 1 - distance)。
  スカラーF32 (量子化なし)。HNSWパラメータ: connectivity=16 /
  expansion_add=96 / **expansion_search=64** (5万件計測でrecall@10 0.995。
  24では0.92 — measurements参照)。
- **永続化**: tmp書き込み → rename のアトミック切替。鍵 = asset id
  (u64 key。正数前提)。
- **資源**: 5万×384次元で索引144.4MiB (約3.0KiB/ベクトル)。

### 索引管理 (gento-tauri `vectorindex::VectorIndexService`)

- **起動・有効化時に再読込** (`ensure_ready`): ファイルがあればロード
  (identity照合) → 即検索可能。件数がSQLite embeddingsと不整合なら
  旧索引で検索させつつバックグラウンド全再構築 (§10: 完了後に切替)。
- **破損時**: ロード失敗 → ファイル破棄 → SQLiteからバックグラウンド
  再構築 (進捗イベント)。**索引は派生データ** — 正本はSQLite embeddings。
- **増分**: 埋め込みジョブ完了フック (`EmbeddingService.set_vector_sink`)
  で稼働索引へ即追加。ディスク反映は30秒毎の定期保存とgraceful
  shutdown時のflush (crash時は次回起動の件数照合→再構築が拾う)。
- **モデル切替**: identity照合で旧索引は自動無効化 (検索不可)。新モデル
  のベクトルが溜まれば新規構築。`embedding_model_delete` は索引ファイル
  も破棄。
- **手動再構築**: `vector_index_rebuild` command (バックグラウンド即復帰)。

### ハイブリッド検索 (重み付きRRF — 数式固定)

```text
score(d) = w · 1/(k + rank_sem(d)) + (1-w) · 1/(k + rank_kw(d))
  k = 60 (RRF標準値・固定)
  w ∈ [0,1] (意味重み。w=0 純キーワード / w=1 意味のみ)
  rank_* = 各リスト内の1開始順位。不在 = 項0
並び: score降順・同点はasset id昇順 (決定性)
```

- 意味側: 検索フリーテキスト → worker `embed_query` (query接頭辞。
  モデル未ロード時はレイジーロード、上限45秒で諦め) → USearch top-K。
- キーワード側: FTS MATCH語があれば **bm25順** top-K
  (`keyword_ranked_asset_ids` — FTSをFROMに直接置き`ORDER BY bm25`)、
  なければ一致id昇順。
- **K = 200/側** (固定)。w>0の結果は融合上位 (最大400件) のみ表示。
  w=0は旧キーワード経路 (id昇順・完全互換)。
- **構造フィルタ (tag:/rating:等) は意味側にも適用**: フィールド条件+
  スコープのみのASTで事前SQL収集した対象asset集合をUSearchの
  filtered_search (labels述語) へ渡す。5万規模実測0.55ms
  (measurements参照 — 取得後フィルタではなく前置述語を採用)。
- **フォールバック** (警告付きでキーワード検索へ): 埋め込み無効・索引
  未構築・モデルロード失敗/タイムアウト。§8: キーワード検索は常時機能。

### Commands (すべてasync + spawn_blocking。DTOはcamelCase)

- `search_run(query, filters?, scopeRootId?, scopeCollectionId?,
  semanticWeight?)` → `SearchRunDto` に `kind:"query"` と
  `semanticScores: Record<assetId, number>` (cosine類似度・上位分) 追加。
  `semanticWeight` は0..1にclamp (省略=0=旧挙動)。
- `similar_search_run(assetId, k?)` → 類似画像検索 (対象assetのdocベクトル
  近傍・自己除外・類似度順)。k既定50・上限200。埋め込み不在/索引未構築は
  Err (日本語メッセージ)。表示種別 `kind:"similar"`。
- `search_refresh` → 表示中検索を再実行。**類似画像表示は同assetで再検索**、
  ハイブリッド表示は保存済み意味重みで再実行 (SearchDisplayにkindと重みを
  保持)。
- `vector_index_status` → `{model, ready, indexedCount, sqliteCount,
  building, lastError}`。
- `vector_index_rebuild` → 全再構築を予約 (即時status復帰)。

### Events (payloadはJSON文字列)

- `vector-index:progress` `{"done":N,"total":M}` — 構築進捗 (4ページ毎)。
- `vector-index:ready` `{"model":"...","count":N}` — 構築/ロード完了。
- `vector-index:notice` (string) — 構築失敗等。

### UI統合

- 検索バー: 意味重みスライダー (0..100%・5%刻み。「キーワード⇔意味」)。
  埋め込み無効・索引未構築時はdisabled + 理由title。
- InfoPanel: `semanticScores` に「意味 xx%」(類似画像表示では「類似度 xx%」)、
  「類似画像を検索」ボタン。グリッドコンテキストメニューにも同項目。
- ヘッダー: 索引構築進捗表示 (`索引構築中 x / y`)。
- 一致根拠 (検索一致セクション) はハイブリッド表示でもキーワード側の
  FTS根拠を表示。

### 実測 (docs/dev/measurements/wp12-usearch-50k.md に詳細)

- 5万×384: 検索 p50=0.35ms / p95=0.45ms / max=0.45ms (acceptance
  <0.5秒を3桁余裕でクリア)。構築16.4s (単一バックグラウンドスレッド)。
  メモリ144.4MiB。保存0.05s・ロード0.06s。
- 実モデル (ruri-v3-70m・WP-11評価セット): 索引top-3 == 総当たりtop-3
  が100%一致、Recall@3 = 0.8333 (総当たりと同一 — 索引化で品質低下なし)。
- 構築中もstatus応答最大4.2ms (96回サンプリング) — UI非ブロック実証。

## 47. WP-13: バックアップと復元

### 概要 (設計判断の記録)

- **2種類のバックアップ**:
  1. **フルDBバックアップ** (`backup_full_db`): `VACUUM INTO` でDB全体を
     同ディレクトリ内の**一時名** (`{最終名}.tmp-{pid}`) へ書き出し、
     integrity_check検証 → データ`sync_all` → 最終名への**原子的リネーム**
     で公開する (クラッシュや中断が最終名の世代を汚さない。一時名は
     retention・復元対象外 — `list_full_backups` は `.tmp-` を含む名を
     除外)。検証・同期・リネームの失敗はいずれも一時名を掃除する。
      **原子rename と durable publication の区別**: renameは可視性の
      原子性のみを保証するため、公開前にファイル内容の`sync_all`、公開後に
      親ディレクトリのsync (best-effort) を行う。**クラッシュ後の永続性は
      FS/プラットフォーム依存** (親dir syncがno-op・エラーになるFSがあり
      得るため — rename原子性の保証とは区別される)。出力先は既定
     `db-backups/` (`BACKUP_DIR_NAME`)。ファイル名
     `library-full-{YYYYMMDDTHHMMSSmmmZ}.sqlite3` (タイムスタンプ先行・
     固定幅 = 辞書順が時系列。migration用 `library-pre-*` とは接頭辞で
     分離し相互に回収しない)。VACUUM INTOはreader接続の
     `PRAGMA query_only` に拒否されるため、専用の短命接続で実行する
     (writerロックを長時間専有しない)。
  2. **`.gento` ポータブルバックアップ** (`backup_portable_create`):
     登録ルート単位の**ディレクトリ形式** (単一アーカイブは却下 —
     圧縮依存を増やさずJSONLの部分検査・ストリーム書き出しが容易)。
     画像ファイル本体は含まない (保守メタデータのみ)。作成は一時
     ディレクトリ `.gento-tmp-{uuid}` へ各ファイルを`sync_all`付きで
     書き、tmpディレクトリもsync (best-effort) してから最終名へ原子的
     リネームで公開 (リネーム失敗時もtmpを掃除する)。
- **自動フルDBバックアップ**: `app_settings` の `backup.auto_full_db`
  (`off`|`daily`|`weekly`、既定 `off`) + `backup.retention_generations`
  (既定3)。起動時+1時間毎の背景チェック (`backup.last_auto_run` 内部キー
  で前回実行epoch msを管理)。
- **`.gento` は敵対的入力 (digest完全性規約)**: digestはバックアップ自身が
  再計算可能なため、整合性だけでは防御にならない。`open_portable_backup`
  は以下を強制する:
  - `complete:true` + 全ファイルBLAKE3 digest一致 (不完全 = 拒否)。
  - `file_digests` は必須2ファイル (`metadata.jsonl` /
    `collections.json`) の**過不足なし一意** (欠落・重複・未知名は
    `DigestMismatch` で拒否 — 部分集合では認証されない要素が復元経路に
    混入するため)。
   - バックアップディレクトリの**実ファイルが期待集合 (上記2つ +
     `backup-info.json`) と完全一致** (manifestに無い実ファイル = 改竄)。
     列挙エラーは1件でも即失敗 (fail-closed)。
  - `backup-info.json` の件数 (asset/file/collection) が実解析行数と一致。
  - asset/file/collection UUIDの重複拒否。
- **rel_pathパス検証規約**: 復元側はすべてのrel_pathを単一の検証関数
  (`decode_rel_checked`) で解釈する。拒否対象 = 不正な百分号エスケープ、
  NUL、絶対パス (unix `/`・windows prefix)、windows prefixの文字列表現
  (drive letter `C:` / UNC `\\` — 現行linux開発でも拒否)、`..` 親
  コンポーネント (percent-encoded `..` も復号後に同一判定)、空・`.`のみ。
  `.` は正規化して除去し `Component::Normal` のみで再構成した相対パスを
  ルートへjoinする。**symlinkはフォローしない (中間コンポーネント含む
  全経路 — FG-WP13-2)**: symlinkは「行方不明」として報告する (fileopsの
  3分類規約に同じ)。実体ハッシュ計算は `gento_core::open_regular_beneath`
  によるルートdir fdからの全コンポーネント歩行で開く (Linux:
  `openat2` + `RESOLVE_BENEATH|RESOLVE_NO_SYMLINKS`。非対応時と非Linux
  unix: `openat` + `O_DIRECTORY|O_NOFOLLOW` のfd歩行) — 中間symlink
  脱出 (`root/linked-dir/image.png` 型) を構造的に遮断し、開いたfdの
  fstatが正本になる (経路は一度だけ解決)。検証不能・非通常ファイルは
  登録しない。
- **復元は2フェーズ + トークン束縛された確定plan**:
  (a) `backup_restore_plan` = dry-run。マッチング優先度は
  **UUID完全一致 → content_hash一致 → rel_path一致**。差分
  (追加/更新/同一)・衝突・行方不明・同一ハッシュ複数件を**レポートのみ**返す。
  (b) `backup_restore_execute` = 単発確認トークン必須 (WP-7 FG-WP7-2の
  intent-bound tokenパターン踏襲。5分失効・1回限り・不一致/流用/再利用は
  全て拒否)。実行シーケンス:
  1. 実行時点でplanを再計算し、意図ダイジェスト
     (`restore_intent_digest` = backup集約digest + root uuid + 対象root id +
     全entry (uuid, match種別, action, 解決済みasset id, content_hash,
     file rel_path) + summary件数 + コレクション (uuid, will_create,
     項目数, 解決可能項目数) を長さ前置連結してBLAKE3) がトークン発行時と
     一致することを確認して**トークンを消費**する。
  2. 消費済みトークンが保証する確定planを適用する。適用は**単一writer
     トランザクション内**で現状からplanを再計算し、ダイジェストが再び
     一致する場合のみ行われる (手順1と適用の間のDB変化は
     `RestoreError::PlanChanged` =「確認の後にライブラリの状態が変化
     しました」で中止。トランザクションはロールバックされDBは無変更)。
     表示用フィールド (reason/display_name/on_disk) は意図ダイジェストの
     同値判定から意図的に除外する (適用に影響する実体状態は適用tx内で
     改めて検証される)。
- **推測して適用しない**: 曖昧マッチ (同一内容が現DBで複数箇所) と
  行方不明 (バックアップにのみ存在し現DBにもディスクにも無い/不明) は
  スキップして結果に報告する。復元は現DBのデータを**削除しない**
  (追加・更新のみ)。
- **埋め込みは再生成扱いに固定** (バックアップに含めない)。サムネイル・
  FTS・USearch索引も対象外 — FTSは復元触ったassetの文書を
  `rebuild_docs_for_assets` (WP-8経路) で即時再構築、サムネイルはキャッシュ
  ミス再生成、USearchはSQLite embeddings正本からの再構築 (WP-12)。
- **キャプション復元の挙動**: 現行行とbodyが異なるkindは現行を履歴
  (`current=0`) へ降格してバックアップ内容を新現行へ挿入する
  (手動編集保護フラグ・sourceはバックアップ側を保存)。タグは正規化名で
  併合 (現DB側のタグを削除しない)。コレクションはuuid一致なら項目
  マージ (冪等)、無ければ同名で新規作成 (smartのquery_jsonは検証してから
  そのまま保存)。

### `.gento` ディレクトリ構成 (format_version 1)

```
{root名}-{YYYYMMDDTHHMMSSmmmZ}.gento/
├── backup-info.json   # 最後に書かれる (complete フラグの意味)
├── metadata.jsonl     # 1行1レコード: {"type":"asset",...} / {"type":"file",...}
└── collections.json   # {"collections":[{uuid,name,kind,query_json,items:[...]}]}
```

- `backup-info.json` schema:

```json
{
  "format_version": 1,
  "api_version": 1,
  "app_version": "0.x.y",
  "created_at": "2026-09-02T00:00:00.000Z",
  "root_uuid": "…36hex…",
  "root_display_path": "/data/photos",
  "asset_count": 3,
  "file_count": 4,
  "collection_count": 2,
  "file_digests": [
    {"name": "collections.json", "blake3": "…64hex…"},
    {"name": "metadata.jsonl",  "blake3": "…64hex…"}
  ],
  "digest": "blake3:…64hex…",
  "complete": true
}
```

- `digest` = `"{name}:{blake3hex}\n"` 行を名前順連結したもののBLAKE3
  (`backup-info.json` 自身は対象外)。読み取りは format_version ≤ 1 を
  要求し、将来は `migrate_to_current` フックで変換する (現状v1のみ)。
  **`file_digests` は必須2ファイルの過不足なし一意であることを要求する**
  (欠落・重複・未成名は拒否 — digest自体は再計算可能なため完全性強制が
  本質)。`asset_count`/`file_count`/`collection_count` は実解析行数との
  一致を要求する (info自体はdigest対象外のため)。
- `metadata.jsonl` の asset行: uuid / content_hash / hash_algorithm /
  byte_size / mime_type / width / height / has_alpha / is_animated /
  frame_count / rating / triage / favorite / tags[] (display_name,
  normalized_name, source, manually_edited) / captions[] (kind, body,
  source, language, manually_edited — 現行行のみ)。
- file行: uuid / asset_uuid / **rel_path (百分号エスケープ正準形 —
  非UTF-8バイトも `%XX` で完全保持)** / display_name / extension /
  byte_size / mtime_ms / mtime_ns / os_file_id `[dev, ino]` / scan_state。

### plan応答のマッチング分類 (`matched`) と action

| matched | 意味 | action |
|---|---|---|
| `uuid` | asset UUID完全一致 (内容も同一) | `same` / `update` |
| `hash` | content_hashで再関連付け (パス移動後) | `same` / `update` |
| `path_add` | 現DBに無いがディスクの同パスに内容一致 | `add` |
| `conflict` | 同一UUIDに別内容 / バックアップ経路に別内容 | `skip` |
| `missing` | バックアップのみ存在・現DBにもディスクにも無い/不明 | `skip` |
| `ambiguous` | 同一内容が現DBで複数箇所 (対応づけ不能) | `skip` |

### Commands (すべてasync + spawn_blocking。DTOはcamelCase)

- `backup_portable_create(rootId, destDir?)` →
  `{path, assetCount, fileCount, collectionCount}`。`rootId` は10進文字列。
- `backup_full_db(destDir?)` → `{path, keptGenerations}`。
- `backup_default_dir()` → string (db-backups/ の絶対パス。UI初期値用)。
- `backup_restore_plan(backupDir, targetRootId?)` →
  `{rootUuid, rootDisplayPath, createdAt, targetRootId, targetRootPath,
  entries[], collections[], summary{same,update,add,conflict,missing,ambiguous},
  confirmToken}`。`targetRootId` 省略時はバックアップroot uuid一致 →
  ルート1つの2段で解決 (不可ならErr「復元先のルートを解決できません」)。
  entries[]: `{assetUuid, contentHash, displayName, currentAssetId,
  matched, action, reason, files:[{backupRelPath, currentRelPath, onDisk}]}`。
  collections[]: `{uuid, name, kind, willCreate, itemsTotal, itemsResolvable}`。
- `backup_restore_execute(backupDir, targetRootId?, confirmToken)` →
  `{applied[], skipped[], collectionsCreated, collectionsMerged,
  collectionItemsAdded, collectionItemsUnresolved, ftsRebuilt}`。
  applied[]: `{assetUuid, action, changed[]}` (changedは
  rating/triage/favorite/tags/captions/created)。
  トークン無効・意図不一致は「復元の実行には確認が必要です…」、
  確認後のDB変化は「確認の後にライブラリの状態が変化しました。計画を
  やり直してから実行してください」(PlanChanged) としてErr。
- `backup_settings_get()` → `{autoFullDb, retentionGenerations}`。
- `backup_settings_set(autoFullDb, retentionGenerations?)` → 同上
  (頻度は `off|daily|weekly`、世代数は1〜100で検証)。

### Events

- なし (作成・復元はcommand応答で完結。進捗イベントは将来課題)。

### UI統合 (最小限 — WP-15で本格化)

- LibraryPanel: ルート行ごとに「バックアップ」(BackupDialog: 保存先
  テキスト入力+フォルダツリー、既定値 `backup_default_dir`)。
- LibraryPanel: ペイン単位に「復元」(RestoreDialog: .gento選択ツリー →
  planサマリ+警告+内訳 → 確認トークン付き実行 → 結果レポート) と
  「DBバックアップ」(既定先へ即時作成)。
- 復元実行ボタンは `planHasWork` (更新/追加/コレクション作業あり) で
  ゲーティング。衝突・行方不明・曖昧は警告領域に列挙。

## 48. WP-14: SD学習用エクスポート

### 概要 (設計判断の記録)

- **入力は解決済みasset id列** (10進文字列。フロントエンドで選択/検索結果/
  コレクションcontentsから解決)。サーバー側で重複排除 (**1 asset = 1出力**)
  し、asset uuid辞書順で処理順を固定する (命名割当の決定論)。
  `parse_stable_asset_ids` と同一ガード (Library系表示 + 仮ID拒否)。
- **代表file選択の決定論**: 同一assetに複数file (重複登録) がある場合は
  `scan_state='present'` の最小file id (presentが無ければ全体の最小id) を
  代表にする。present優先は「移動で旧経路がmissingになった直後」に
  実在する複製へフォールバックするための解釈 (PM決定「file id最小」の
  精緻化 — 反証条件: 手動タグ順序UI等の要望が出た場合に再検討)。
- **`.txt`内容**: 現行caption (`current=1`) があれば本文をそのまま。
  複数kindの解釈は **`sd_tags` 現行 → `description` 現行 → タグ列** の
  優先順 (SD学習用途に特化したkindを先頭に。仮定として記録 — 反証条件:
  ユーザーがkind選択UIを求めた場合)。タグ列は `list_asset_tags` 契約と
  同一の `normalized_name ASC` 順で display_name を `", "` 結合
  (「SDタグ列の順序を維持」= GEN-TOが生成する順序を壊さず決定論的に)。
  caption・タグとも無い場合は**空の`.txt`** を出力し、manifestの
  `caption_source: "none"` で記録する (件数から除外しない)。
- **命名**: 出力ファイル名 = 代表fileの `display_name` (拡張子含む)。
  同一dir内で異なるassetが同名になる場合は、uuid昇順で先頭がbase名を
  維持し、以降 `{stem}-{n}.{ext}` (n=1,2,…。拡張子無しは `{stem}-{n}`)。
  割当は「そのexport実行内でのみ使用される名前集合」に対して行う
  (既存ファイルは割当に考慮しない — 冪等再実行で同一割当を保証するため)。
  `.txt`名は最終画像名から導出 (`stem + ".txt"`。先頭dotのみの名前は
  拡張子無し扱い)。フロントエンド (`lib/export.ts` の `txtNameFor` /
  `collisionSuffixName`) は同一規則の薄い表示ミラー。
- **元ファイル・DBは完全不変**: ソース読取はすべて
  `gento_core::open_regular_beneath` (登録ルートfdから全コンポーネント歩行。
  FG-WP13-2)。DB参照は `with_reader` (reader接続は `PRAGMA query_only` で
  書き込みSQLを構造的に拒否)。ソースは読み取り専用openのためmtimeも不変。
- **出力経路の包含 (FG-WP16-3 B1)**: 出力dirはユーザー指定パスを
  `O_DIRECTORY|O_NOFOLLOW` で開いてfd固定する (**root最終成分がsymlink
  なら拒否** — 日本語エラーで報告)。preserve構造のサブディレクトリは
  `openat(.., O_DIRECTORY|O_NOFOLLOW)` の成分歩行で開き、不在なら
  `mkdirat`→再open (作成とopenの競合安全。既存成分がsymlinkならELOOP
  拒否)。ファイル書き出し (tmp `O_CREAT|O_EXCL` → sync →
  `renameat2(RENAME_NOREPLACE)`) も同じdir fd相対で行う。このため
  `DEST/sub -> /outside` のような**出力dir内の既存symlinkを辿った外部
  書き出しは構造的に起きない** (該当項目はfailedとして報告され、外部に
  は1ファイルも作られない)。最終ファイル名が (外部実体への) symlinkの
  場合はConflict扱い (リンクもリンク先も無傷)。
- **無断上書き厳禁 (既存出力との衝突)**: 出力先に同名ファイルがある場合、
  サイズ一致ならBLAKE3ストリーム比較し、**内容一致 = 「既に書き出し済み」
  でskip (冪等)**、不一致 = skip + 衝突報告 (manifestと結果DTOに記録)。
  比較の正本は**ソース実体** (open_regular_beneathで開いたfdの内容) で
  あり、DBの content_hash ではない (スキャン後の変化に強い)。
  新規書き出しは tmp書き (`.{名前}.gento-tmp-{32hex}-{6桁seq}` —
  WP-7 fileopsと同一の厳密形式。出力先が登録ルート内なら起動時掃除の
  対象) → `sync_all` → no-clobber rename による原子公開。
  上書き競合 (renameのDestinationExists) は失敗として扱う。
- **フォルダ構造**: `preserve` = 代表fileのrel_path親dir構造を出力先に再現
  (成分は `Component::Normal` のみ許可 — `..` 等は構造的に拒否して
  plan失敗扱い)。`flat` = 全て直下 (衝突はsuffixで解決)。
- **dry-run → 実行の2段**: `export_preview` は書き込み一切なし
  (出力先が存在しなくても成立し、作成もしない)。既存衝突分類
  (`none|identical|conflict`) とcaption先頭80文字を返す。UIは
  dry-runを1回取得しないと実行不可 (`canExecute`。構造変更でdry-runは
  無効化)。実行は計画を組み直してから項目単位で処理 (クライアント由来の
  planは信頼しない — fileops/WP-13と同一規約)。
  **既知挙動 (previewの安全側不一致 — 修正しない)**: previewは読み取り
  専用の `OutputDir` で分類するため、開けない/安全でないパス (symlink済み
  subdir等) は「不在」として報告され (`toWrite` に加算)、実行側は同じ
  パスを拒否してfailedにする。つまりpreviewの予測が実行結果と不一致に
  なり得るが、これは読み取り専用性・安全性を優先した既知の安全側挙動。
- **部分失敗・キャンセル**: 単一項目の失敗 (ソース不在・書込エラー) は
  続行し manifest に記録。キャンセル (`export_cancel` → AtomicBool) は
  per-item境界で確認し、中断時もそれまでの成果 + manifestは残す
  (`cancelled: true` + 未処理件数)。実行排他は1件 (2件目の同時実行は
  「別のエクスポートが実行中です」)。実行中のpanicで排他状態が残留した
  場合は以後のエクスポートを拒否し続ける (安全側 — 再起動で回復)。
- **manifest** (`export-manifest.json` を出力dirへ): tmp書き→sync→
  **`renameat2(RENAME_NOREPLACE)` で出版** (FG-WP16-3 N3)。
  **既存ファイルは所有者を問わず (GEN-TO自身の旧manifestも含め) 一切
  削除・上書きしない** (FG-WP16-4 R3 — 「所有確認→unlink」間に他actorが
  差し替えたユーザーファイルを消しうるTOCTOUを、unlink自体をなくす
  方針で解消)。正規名 `export-manifest.json` が空きならそこへ出版し、
  占有されていれば回避名 `export-manifest-{run uuid}.json` へNOREPLACE
  出版する (既存は不接触。応答の `manifestPath` が実際のパスを報告 —
  再実行のたびに回避名のmanifestが蓄積するのはこの方針の帰結)。
  snake_case (backup artifactと同一規約)。内容: format_version / created_at /
  cancelled / structure / dest_dir / **asset_count (処理済み + plan失敗 +
  キャンセル未処理 = 要求した一意asset数。DTOの `total` と同一定義 —
  FG-WP16-3 N2)** / summary (exported, identical, conflict, failed,
  planning_errors, cancelled) / entries[] (asset_id, asset_uuid,
  display_name, source_display, output_rel, caption_source,
  image ("written|identical|conflict|failed") + error, txt + error) /
  errors[] (plan失敗)。

### Commands (preview/executeはasync + spawn_blocking。DTOはcamelCase)

- `export_preview(assetIds, destDir, structure)` →
  `{destDir, structure, entries[], summary}`。
  entries[]: `{assetId, assetUuid, displayName, sourcePath, outputRelPath,
  txtRelPath, imageExisting ("none|identical|conflict|n/a"), txtExisting,
  captionSource ("caption_sd_tags|caption_description|tags|none"),
  captionPreview (先頭80文字), error (plan失敗理由。成功時null)}`。
  summary: `{total, toWrite, alreadyExported, conflict, error}`。
- `export_execute(assetIds, destDir, structure)` →
  `{manifestPath, cancelled, summary, failures[], conflicts[], issuesTruncated}`。
  summary: `{total, exported, identical, conflict, failed, planningErrors,
  cancelled}` (total = 処理済み + plan失敗 + キャンセル未処理)。
  **summaryのconflict/failedはtruncation前の全件から計算する**
  (FG-WP16-3 N1 — 配列だけが先頭100件に切り詰められ、`issuesTruncated`
  がtrueになる)。
  failures[]/conflicts[]: `{assetUuid, displayName, outputRelPath, reason}`
  (各々先頭100件まで。全量はmanifest)。
  `structure` は `"preserve" | "flat"`。
- `export_cancel()` → void (協調キャンセル。per-item境界で効く)。

### Events

- `export:progress` — 実行中、項目ごとに
  `{processed, total, assetUuid, status}` (status は
  `"exported" | "identical" | "conflict" | "failed"`)。

### UI統合

- グリッド選択のコンテキストメニュー「SD学習用にエクスポート…」
  (stable AssetIdを持つ項目 = Library系表示のみ)。
- コレクション表示中のツールバー「エクスポート…」(表示中コレクションの
  全項目をmanual/smartそれぞれのitems APIからページング解決)。
- ExportDialog: 入力源表示 + 出力先 (テキスト入力+フォルダツリー —
  BackupDialogと同一規約) + 構造toggle + dry-runプレビュー表
  (出力パス/.txt/既存分類/caption先頭) + 実行・キャンセル + 結果サマリ
  (失敗・衝突・manifestパス表示)。

## 49. WP-15: 設定・診断・プライバシー

### 設計判断 (PM決定の記録)

- **設定はDB migrationなし**: `app_settings` キー値テーブルに名前空間付き
  キー (`general.*` / `cache.*` / `search.*` / `backup.*` 既存) で保存する。
  schema version相当は `settings.version` 値のみ (PM決定「migration禁止」)。
  新規キーの値はJSON直列化 (`"warn"` / `2147483648`)。既存backup系キー
  (`backup.auto_full_db`=`off` 等の素の文字列) は既存コマンド互換のため
  素の形式のまま受け入れる (accessorは両形式を解析)。
- **破損値は既定値 + warnログで継続** (受け入れ「設定破損時に安全な既定値
  で起動」)。破損検知件数・キー一覧は `diagnostics_snapshot` の
  `db.settingsCorruptCount` と `settings_get` の `corruptKeys` から観測。
- **既定値は現状挙動と同値**: `cache.thumbs_max_bytes` 既定 = 2 GiB
  (gento-image `cache::DEFAULT_MAX_BYTES` と同値。値はgento-db側に重複
  定義され、tauri層テスト `dto_defaults_and_cross_crate_default_pin` で
  同値を固定)。
- **サムネイルキャッシュ**: 上限変更は即時適用 (`ThumbnailCache::
  set_max_bytes`。縮小時はその場で目標容量95%までLRU prune)。削除
  (`clear_thumbnail_cache` 既存) は**通常ファイルのみ** (symlinkは辿ら
  ない・削除もしない — リンク先の実体を絶対に触らない)。起動時回復も
  symlinkを台帳登録しない。再構築は需要駆動再生成で担保 (既存テスト)。
- **診断は全てread-only**。`PRAGMA quick_check` のみFTS5整合性検査の内部
  書き込みが必要なため例外的にwriter接続で実行 (`Db::diagnose` と同一理由)。

### 秘密・GPS・パスのredaction規約 (最重要)

- 監査結果: APIキー・GPSの既存泄漏経路は無し (ApiKeyのDebug/Display伏せ字、
  SecretStoreエラー文の鍵不含、AiErrorがAuthorization値を整形しない、
  request_summaryの鍵除外 — いずれも構造的)。WP-15は以下の**構造的防護を
  追加**した:
  - `gento_core::redact`: `redact_key` (先頭4字+マスク)、`scrub_text`
    (Bearer/Authorization/sk-系/api_key=/GPS座標らしい高精度小数5桁以上の
    除外)、`scrub_paths` (絶対パス→`<path>`。unix `/` 始まりに加え
    **Windowsドライブ (`C:\…` / `C:/…`) とUNC (`\\server\share\…`) も
    対象**。**空白を挟んで後に区切り (`/` `\`) が続くトークンはパス候補
    として丸ごと** scrubする (`/home/u/My Photos/x.png` 等。誤爆<漏れの
    方針。URLは `:` が継続を打ち切るため保持 — FG-WP16-3 B4)。
    **既知の限界 (WP-17申し送り — FG-WP16-4で文書化)**: ①パス構成文字の
    判定がASCII範囲のみのため、部分scrub後の非ASCII成分
    (例: `/home/利用者/file.png` の `利用者/…` 部分) は漏れ得る
    ②空白継続heuristicは後に区切りが続く散文を過剰scrubし得る
    (「漏れ<誤爆」方針の既知の代償)。
  - `MetadataColumns`/`AssetMetadataParsed` のDebugを手動実装 (GPS列は
    `<gps>`、rawスロットはバイト数要約に伏せる — `{:?}` 混入時の防護)。
  - AIジョブ失敗時のwarnログ (scrub通し)。panic payloadのscrub。
- **診断bundle** (`diagnostics_bundle_create`): ユーザー指定dirへJSON 1ファイル
  (tmp書き→sync→原子rename)。内容: format/app/api/schema version、設定
  (鍵素材は構造的に不含。プロファイル・鍵参照も対象外)、診断スナップショット、
  直近ログ末尾100行 (メモリringから)。**出力前に**: (1) 全文字列をscrub_text、
  (2) 既知dirを実パス→プレースホルダ (`<root:N>`/`<db>`/`<data>`/`<cache>`/
  `<log>`/`<thumbnails>`/`<models>`/`<home>`、長い順)、(3) 残る絶対パスを
  `scrub_paths` で `<path>`。APIキー・GPS座標・実パス・画像内容 (base64
  data URL) が一度も出現しないことはテストで実証
  (`bundle_excludes_secrets_gps_and_real_paths` — 鍵・GPS・実パスを
  意図的に混入した状態で生成)。
- **ログ**: stdout形式は従来どおり (計測ドライバ互換)。加えて直近512行の
  メモリring (bundle専用)。ログレベルは `general.log_level` 設定で実行時
  変更 (tracing reload handle。起動時は `GENTO_LOG` env優先)。

### Commands (DTOはcamelCase)

- `settings_get()` → `{schemaVersion, corruptKeys[], general:{logLevel},
  cache:{thumbsMaxBytes, usedBytes, maxBytes}}`。DB未接続でも既定値で応答。
- `settings_set_log_level(level)` → SettingsDto。levelは error|warn|info|debug。
  永続化 + tracing reload (即時反映)。
- `settings_set_thumbs_max_bytes(bytes)` → SettingsDto。範囲 64MiB〜1TiB
  (範囲外は拒否)。永続化 + キャッシュへ即時適用。
- `diagnostics_snapshot()` → 5領域:
  `{db:{dbOpen, integrityCheck, schemaVersion, dbBytes, walBytes,
  settingsCorruptCount, dbPath}, fts:{expectedDocs, actualDocs, missingDocs,
  orphanDocs, duplicateDocs, staleContents, consistent, lastRebuildAt},
  usearch:{model, indexedCount, sqliteCount, stale, dimensions, ready,
  building, lastError}, models:{enabled, packages[{modelId, state,
  embeddingDim, license}], workerPhase, workerRestarts, workerLastError,
  loadedModel}, jobs:{byType[{jobType, status, count}],
  oldestPendingAgeSeconds}}`。
- `diagnostics_bundle_create(destDir)` → `{path}`。
  ファイル名 `gento-diagnostics-{RFC3339}.json` (同一時刻印の先任者が
  いれば `-連番`。命名は実行lock + uuid付きtmp + `rename_noreplace` で
  競合安全 — 並行生成でも各成果物が別名で残り上書き失効しない。
  FG-WP16-3 N4)。
- 既存 `clear_thumbnail_cache` / `backup_settings_get` / `backup_settings_set`
  はそのまま (SettingsDialogのキャッシュ・バックアップタブから利用)。

### FTS最終再構築時刻

- `fts_rebuild` / `fts_repair` コマンドと走査後自動修復が成功時に
  `fts.last_rebuild_at` 設定 (RFC3339) を記録する。診断の
  `fts.lastRebuildAt` から読む (未実施 = null)。

### AI endpoint識別 (フロントエンド実装)

- `classifyEndpoint(baseUrl)` (lib/settings.ts): host分類
  `local` (localhost/127.0.0.1/::1) / `private` (RFC1918 + 169.254/16 +
  IPv6 link-local/ULA) / `external` / `invalid`。SettingsDialogのAIタブと
  AiPanelのプロファイル行に表示。ルート毎の ai_enabled/send_exif/send_gps
  はSettingsDialog AIタブに行列で可視化 (読み取り専用。初期状態は全オフ =
  `RootSettings::default` 回帰テストで固定済み)。

### UI統合

- ヘッダー歯車「⚙ 設定」→ SettingsDialog (タブ: 一般/表示/キャッシュ/
  AI/検索/バックアップ/診断)。表示タブは既存pref (folder-prefs) の現状値
  の表示のみ。診断タブは5領域 + 更新ボタン + bundle生成 (出力先dir入力)。
