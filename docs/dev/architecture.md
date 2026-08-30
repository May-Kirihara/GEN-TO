# アーキテクチャ (WP-1時点の実装に即した要約)

- 作成日: 2026-08-30 (WP-0。WP-1で gento-db 追加。WP-2で library系追加)
- 目的: crate責務・依存方向・公開APIの要点・devtools featureの境界を
  実装に即して1枚で把握するためのもの。詳細な契約は
  [api-contract.md](api-contract.md) を参照。

## 依存方向

```
gento-core  (ドメイン型・repository trait。Tauri非依存)
    ↑           ↑
gento-image  gento-db   (画像パイプライン / SQLite基盤。いずれもgento-coreに依存)
    ↑           ↑
gento-tauri (Tauri thin adapter。gento-core + gento-image + gento-dbに依存)
```

Tauriに依存してよいのは `gento-tauri` のみ。逆方向の依存
(gento-core/image/db → tauri) は禁止。`gento-core` はDB実装にも
依存しない (repository traitのみ定義し、実装は `gento-db`)。

## 各crateの責務と公開APIの要点

### `crates/gento-core` — ドメイン型・カタログ・repository抽象

- `AssetId` (u64新type。IPCでは10進文字列化してJS安全整数問題を回避)
- `HealthInfo` (`health` commandのDTO。`api_version`・`db_open` 含む)
- `GENTO_API_VERSION` 定数と `AppDirs` (data/cache/logディレクトリの
  解決結果を保持するだけの構造。解決はアダプター側。WP-1のSQLite DBは
  `data_dir` 直下)
- `jobs`: `SizeClass` / `Priority` (ジョブ系の基本型)
- `catalog`: `ImageRecord`・`CatalogState` (安定順序・generation付き
  ページング・上限500件強制)・ページDTOとエラー形状
- `catalog::virtual_catalog` (**devtools**): 5万件仮想カタログ生成
- `repo` (WP-1): 永続化repository trait (`LibraryRootRepo` /
  `FileRepo` / `AssetRepo`) とレコード型 (`LibraryRootRecord` /
  `FileRecord` / `AssetRecord`)、`ScanState`・`TriageState`・
  `HashState`・`RepoError`、`path_is_within` (親子パス判定)。
  SQLite実装はgento-db (依存方向: gento-db → gento-core)。
  WP-2で設定列 (`RootSettings`)・`OsFileId` (Linux dev+inode)・バッチ
  API (`upsert_files_batch`/`set_scan_state_batch`)・移動
  (`relocate_file`)・ページング (`list_files_paged`)・JOIN一覧
  (`list_present_joined`)・`create_or_get_asset` を追加
- `library` (WP-2): 登録ルートの走査・同期エンジン。**Tauri・rusqlite非依存**
  (trait注入: `RepoStore` (実装=gento-db `Db`) / `ScanFs`
  (実装=gento-image `PlatformFs`) / `AllRepos`)。差分スキャン
  (`scan_root`: size+mtime高速判定・os_file_id移動検出・未参照missing化・
  小バッチcommit)、`hash_and_link_file` (BLAKE3→Asset統合)、
  `WatchBuffer` (監視イベントのパス単位結合)、ライブラリAssetId符号化
  (`library_asset_id`/`provisional_file_asset_id`。api-contract §19)

### `crates/gento-db` — SQLite基盤 (WP-1。メタデータの正本)

- `Db::open(path)` / `Db::open_with(path, DbOptions)`: 親ディレクトリ
  作成 + migration適用まで行う。新規作成・既存オープン両対応。
  DBの `user_version` が本バイナリの最新より新しい場合は拒否する
  (旧バイナリでの未来schemaアクセス防止)
- migration (migrate.rs): `PRAGMA user_version` 管理・順方向のみ・
  1件=1トランザクション・失敗時rollback・冪等スキップ。
  データ保有DBへの未適用migrationがある場合、適用前に `VACUUM INTO`
  によるWAL安全backupを作成し、integrity_checkで検証、世代保持
  (既定3世代、`db-backups/` 配下 `library-pre-{時刻}-v{N}.sqlite3`。
  時刻は固定幅19文字で**先頭** — 辞書順=時系列が成立し、version桁数
  の違いで世代破棄順が崩れない)。`user_version=0` で非管理オブジェクト
  (テーブル・view・trigger) が存在するDBは拒否
- migration #1 (schema.rs): 12テーブル (`library_roots` / `assets` /
  `files` / `tags` / `asset_tags` / `captions` / `collections` /
  `collection_items` / `asset_metadata` / `jobs` / `file_operations` /
  `embeddings`) と検索用インデックス。DDLはdocs/data-model.md §2〜§12
  準拠。
- migration #2 (WP-2): indexのみ — `files(root_id, scan_state)` と
  `os_file_id` 部分index (列追加不要。設定列・os_file_id列は#1に存在)`library_roots` はルート設定列 (`watch_enabled`/`ai_enabled`/
  `send_exif`/`send_gps`/`settings_json`) を持つ。AiProfile/AiRunは
  WP-10の追加migration — `ai_run_id` 列もその時点で `ALTER TABLE …
  ADD COLUMN … REFERENCES ai_runs(id)` として追加する (既存列へのFK
  付与にはテーブル再構築が必要なため、#1には列自体を置かない)
- `SqliteRepos`: gento-coreの3 repository traitのSQLite実装。
  WP-2が必要とする最小範囲 (root追加/一覧/有効化/解除/親子検出雛形、
  file upsert (root_id+rel_pathキー)・scan_state更新・missing一覧、
  asset作成 (content_hash一意)・hash検索・file↔assetリンク)。
  エラー分類: 一意制約→`RepoError::Duplicate`、CHECK/NOT NULL→
  `RepoError::Constraint`、FK違反→`RepoError::Db` (文面で判別)
- `Db::diagnose()`: `PRAGMA integrity_check` (複数診断行を全行収集・
  上限20行で要約)・`foreign_key_check`・schema version・journal_mode・
  テーブル行数・DB/WALファイルサイズ
- `percent_encode_bytes` / `percent_decode_to_bytes`: backup/restore用
  正準表現 (パーセントエスケープUTF-8。WP-13で使用)
- 格納規約: UUIDv7 TEXT 36文字小文字 (一意性のみ保証、単調性は保証
  しない)、DBレコード時刻はUTC RFC3339 TEXT (ファイル由来時刻はepoch ms
  整数)、enumはTEXT + CHECK制約、パスは生バイトBLOB + `path_encoding`
  ('unix-bytes'|'utf8') + 表示名TEXT (lossy) 分離で非UTF-8パスを保持

### `crates/gento-image` — 画像パイプライン (Tauri非依存)

- `scan`: walkdirによるPNG再帰走査 (エラー隔離・バッチsink)。
  WP-2で `scan_images` (MVP 7形式の拡張子判定 + os_file_id収集) を追加
- `fs` (WP-2): gento-core `ScanFs` の実装 `PlatformFs`
  (walk=scan_images・probe=PNG・hash=blake3)
- `hash` (WP-2): ファイル全体のBLAKE3 (1MiBチャンク ストリーミング・
  協調キャンセル。`blake3:<64hex>` 形式)
- `probe`: PNGシグネチャ+IHDRのみの寸法プローブ (全文デコードなし)
- `thumbnail`: デコード (Limits付き) → リサイズ → WebPエンコード。
  `ThumbnailJob` がプール投入単位。`FailureInjection` (**devtools**) は
  決定的エラー注入
- `cache`: キャッシュキー (canonicalパス+size+mtime+クラス+アルゴリズム)、
  シャード配置、2GiB LRU会計 (in-flight予約付き)、一時ファイル+アトミック
  rename、`content_version` (URL `?v=` の内容感応バージョン)
- `pool`: 優先度5段付きワーカープール (dedup・上限4096・協調キャンセル)
- `stats` / `metrics`: 生成時間パーセンタイル、走査計測ログ (`scan metrics` 行)

### `apps/desktop/src-tauri` (`gento-tauri`) — Tauri thin adapter

- `lib.rs`: アプリ本体。`AppDirs` 解決 (dirs crate)、ログ初期化
  (`GENTO_LOG`)、panic hook (panic.log)、SIGTERM graceful終了、
  invoke handler登録 (devtoolsビルドのみ計測commandを追加)、`health`
- `db.rs` (WP-1): DBパス解決 (`--db` / `GENTO_DB` / 既定)・`DbState`
  保持 (setupでオープン。失敗時もアプリ継続)・`db_status` command
- `library.rs` (WP-2): `LibraryController` — スキャン逐次ワーカー
  (単一キューで全体/スコープ差分を直列実行)、BLAKE3ハッシュ専用プール
  (2ワーカー・Background優先度)、ルートごとのnotify (inotify) 監視
  (debounce 400ms → スコープ差分。欠落は全体再スキャンで収束)、
  カタログスナップショット生成 (DB→ImageRecord)、command群
  (`library_roots_*`/`scan_start`/`scan_stop`/`scan_status`)。setupで
  有効ルートの監視開始+初回スキャンを非同期投入、Exitでshutdown
- `catalog.rs`: カタログcommand群 (summary/page/scan) と走査スレッド。
  ページDTOへのversion付与もここ (thin adapter層の責務)。WP-4で
  `open_folder` (閲覧フォルダ切替→一時走査)・`set_catalog_sort`
  (表示ソート)・DTOの情報パネル用フィールド (path/mtime/mime/dbState/
  triage) を追加
- `fsview.rs` (WP-4): フォルダツリー用の直下ディレクトリ列挙
  (`fs_list_dirs`。隠し除外・遅延ロード・権限エラーの文言化) と
  OS連携 (`reveal_in_file_manager` / `open_with_default_app` —
  tauri-plugin-openerのRust APIのみ使用。JS側plugin commandは叩かない
  ためcapability追加なし)
- `thumbnails.rs`: サムネイル生成サービス (要求の受付・dedup・イベント
  バッチング・統計DTO) とcommand群
- `protocol.rs`: ローカル画像プロトコル3種 (gento-thumb/preview/original)。
  URL構造化パース・厳密検証・TOCTOU閉鎖・統計
- `devtools.rs` (**devtoolsのみ**): 計測ブリッジcommand
  (`dev_bridge_enabled` / `devtest_report`) と環境変数ゲート

フロントエンド (`apps/desktop/src`) はSvelte 5 + TS。ストア層
(catalog/thumbnails/grid/viewer/compare) はIPCを注入されたプレーンな
クラスで、Svelteコンポーネントから利用する。WP-4で製品シェル
(`App.svelte` + `lib/shell/*` の3ペイン構成、`lib/keys.ts` のキー割当
一元化、`lib/preview`・`lib/session`・`lib/ui` の各ストア) を追加。

## SQLite運用方針 (WP-1確定。WP-2以降もこの方針に従う)

- **並行性**: 書き込みは単一writer接続に集約 (`Db::with_writer` が
  Mutexで直列化。scan/UI編集/AIジョブ/backupの全書き込みがこの経路)。
  読み取りは分離接続 (`Db::with_reader`。WALなのでwriterをブロック
  しない。idle 2本、貸出集中時のみ追加接続を開き返却時に閉じる)。
  **reader接続は `PRAGMA query_only=ON`** — repo API・生SQLのいずれの
  書き込みもSQLiteが `SQLITE_READONLY` で拒否する (単一writer方針の
  強制。`OPEN_READ_ONLY` フラグにしないのはWALのpager処理
  (-shm書き込み・recovery) に書き込み権限が必要になり得るため)。
  busy_timeout 5000ms (外部プロセスとの競合猶予)
- **PRAGMA**: WAL / `foreign_keys=ON` (全接続で毎回) /
  `synchronous=NORMAL` (WAL組み合わせの標準推奨。アプリ異常終了で
  破損しない。電源断では直近トランザクションを失い得る)
- **配置**: `AppDirs.data_dir/library.sqlite3` (`~/.local/share/gento-spike/`)。
  上書き: `--db <PATH>` 引数 > `GENTO_DB` 環境変数。backupは
  `data_dir/db-backups/`
- **異常時挙動**: DBを閉じずにプロセス死亡 → 次回オープンでWAL自動回復
  (クラッシュシミュレーションテストあり)。migration失敗 → rollback +
  backup保持で元DB不変。DB open失敗 → アプリは続行し `health.db_open`
  / `db_status` で観測。未来schema (`user_version` > バイナリの最新) は
  拒否。非管理DB (user_version=0でテーブル・view・triggerあり) は
  拒否して上書きしない
- **破損・権限・disk full**: integrity_check/foreign_key_checkは
  `db_status` で常時観測可能。disk fullはSQLiteエラーとして各書き込みが
  失敗する (RepoError::Db)。移行時のbackup検証失敗はmigrationを中止する

## devtools featureの境界 (WP-0)

計測・検証専用機能は cargo feature `devtools` (既定無効) で隔離する。
コードは `#[cfg(any(debug_assertions, feature = "devtools"))]` で制御するため:

| ビルド | devtoolsコード | 計測ブリッジ (devtest) |
|---|---|---|
| debug (開発・`cargo test`) | 含まれる | `GENTO_DEV_BRIDGE=1` で有効 |
| release (通常) | **含まれない** | 存在しない (環境変数無効) |
| release `--features custom-protocol,devtools` | 含まれる | `GENTO_DEV_BRIDGE=1` で有効 |

devtools対象: `dev_bridge_enabled` / `devtest_report` / `set_failure_injection`
command、`FailureInjection` (gento-image)、`set_catalog_mode` command と
仮想5万件カタログ (gento-core)、`get_thumbnail_stats` の
`injection_mode`/`injection_rate` フィールド、フロントの開発パネル
(仮想カタログボタン・エラー注入パネル。`dev_bridge_enabled` invokeの
成否で表示を切り替え)。

featureはcrate単位の定義で、`gento-tauri` の `devtools` が
`gento-core/devtools`・`gento-image/devtools` へ伝播する
(workspaceルートのコメント参照)。

cargo直接のreleaseビルドでは `custom-protocol` featureの明示も必要
(tauri CLI `pnpm tauri build` が自動で付与するもの。無いとfrontendDistの
埋め込みではなくvite devサーバー `localhost:5173` を見に行く)。

## アプリケーションディレクトリ (WP-0)

`gento_core::AppDirs` が保持し、`gento-tauri` の `resolve_app_dirs` が解決:

| 用途 | 既定位置 (Linux) | 備考 |
|---|---|---|
| `data_dir` | `~/.local/share/gento-spike` | SQLite DBは `data_dir/library.sqlite3`、migration前backupは `data_dir/db-backups/` (WP-1) |
| `cache_dir` | `~/.cache/gento-spike` | サムネイルは `cache_dir/thumbnails` |
| `log_dir` | `~/.cache/gento-spike/log` | `panic.log` |

`--cache-dir <PATH>` 起動引数はサムネイルルートのみ上書きする
(計測ドライバ互換。data/log位置は動かない)。DB位置の上書きは
`--db <PATH>` または `GENTO_DB` 環境変数 (WP-1。テスト・計測用)。

## ログ・診断 (WP-0)

- 通常ログ: stdout、tracing fmt既定形式 `TIMESTAMP LEVEL TARGET: message`。
  `GENTO_LOG` (EnvFilter書式、既定 `info`) で制御
- panic: 既定フック (stderr) を維持しつつ、`log_dir/panic.log` へ
  「panic情報 + バージョン + OS + APIバージョン」を1行追記 (backtrace無し。
  軽量に保つ)
- 計測チャネル: devtoolsビルドの `devtest:` ターゲットログ (1秒周期)。

## スモーク・ライセンス

- `scripts/release-smoke.sh`: devtools隔離のartifact-level証拠と
  起動・計測シナリオの簡易検証 (README「releaseスモーク」参照)
- `scripts/gen-licenses.sh`: [licenses.md](licenses.md) を再生成
