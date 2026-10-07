# GEN-TO

高速画像表示の技術検証プロトタイプ。Tauri 2 + Svelte 5 + TypeScript + Rust (Cargo workspace) で構成する。
スパイク ([技術検証計画](docs/technical-spike.md)、[結果報告](docs/technical-spike-report.md)、[計測値](docs/technical-spike-results.md)) を母体に、MVP実装 ([MVP実装計画](docs/mvp-implementation-plan.md)) を進行中。

## リポジトリ構成

- `crates/gento-core` — ID・基本ドメイン型・repository抽象・登録ルート走査エンジン・バックオフ計算 (Tauri非依存)
- `crates/gento-db` — SQLite基盤 (migration・repository実装・永続ジョブキュー。メタデータの正本)
- `crates/gento-image` — 画像パイプライン (走査・プローブ・BLAKE3・サムネイル・キャッシュ。Tauri非依存)
- `crates/gento-ai` — OpenAI互換AI統合 (HTTPクライアント・versionedプロンプト・APIキー保管・実行パイプライン。Tauri非依存)
- `apps/desktop` — Svelte 5 + Vite フロントエンドと `src-tauri` (Tauri 2 thin adapter)

## 製品UIシェル (3ペイン、WP-4)

アプリは3ペイン構成の画像ファイラーとして動作する:

- **左ペイン**: タブ切替 (UIUX-1: フォルダ / コレクション / ライブラリの3タブ。
  各タブ独立スクロール・最終選択タブは永続化)。フォルダツリー
  (ホーム/ファイルシステム起点の遅延ロード。未登録フォルダも閲覧可能。
  登録ルートはバッジ表示。**フォルダーの右クリックで「このフォルダーを
  登録ルートに追加」**(UIUX-1。登録済みルートなら「登録を解除」「このルートを
  バックアップ」「ライブラリを再スキャン」) ) + 登録ルート管理 + コレクション/
  保存済み検索 (WP-6/8実装済み: 手動コレクションと条件ビルダー付き
  スマートコレクション=保存済み検索。追加・改名・削除・再評価・件数表示・
  メモ・並び替え。タブ切替でも作成途中の入力は失われない)
- **ヘッダー**: アプリタイトル + DB接続状態ドット + 表示ソースバッジ
  (ライブラリ/検索結果/一時走査) + **UIUX-5: 「◐」ステータスボタン1個**
  (走査進捗・ハッシュ残件・索引構築・監視劣化・アプリ通知を状態色付きの
  ボタン+popoverへ集約)
- **メニューバー (UIUX-1)**: ヘッダー直下に ファイル / 表示 / ツール / ヘルプ。
  一時走査の実行・中止、SD学習用エクスポート、終了 / ペインとサムネイル
  サイズ / AIパネル・設定・バックアップ系・復元・開発パネル (dev build) /
  このアプリについて。キーボード (矢印移動・Escで閉じる) にも対応
- **中央ペイン**: 検索バー (WP-8: 構文入力 `青い髪の少女 tag:night
  rating:>=3 type:png added:2026-01-01..2026-06-30` + GUIフィルター +
  検索範囲選択 + 保存済み検索への保存・再実行。WP-9で画像メタデータキー
  `model:"..." steps:>=30 has:workflow sampler: vae: lora:` を追加 —
  生成画像の埋め込み情報で絞り込み、positive promptは全文検索対象。
  **UIUX-3: 🔍ボタンで1行に折りたたみ可能** (確定済み検索語+scope実態+
  実行中は✕で解除。状態は永続化)。**UIUX-5: GUIフィルターは「フィルター」
  ボタンのpopoverへ格納** (Esc/外クリックで閉じる)。適用中のフィルタは
  チップ行で表示 — ×で個別解除、チップは未確定のドラフト値を反映)、
  パンくず (UIUX-1: 右端の✏ボタンで**パスを直接入力して移動**。絶対パスのみ。
  存在しないパスはエラー通知) + ソート + ビューア種別 + サムネイルサイズ
  のツールバー (UIUX-3: スクロール速度スライダーは**設定ダイアログの表示
  タブ**へ移設 — 既定2.0倍。UIUX-5: ビューア種別はセグメントコントロール、
  サムネイルサイズは「小/大」2値トグル、手動コレクションの位置操作は「…」
  オーバーフローpopoverへ集約。toolbarのエクスポートボタンは撤去 —
  メニューバー「ファイル>SD学習用エクスポート…」が正本)、
  1行notice-strip (UIUX-5: エラー系通知を最優先で1行に集約。揮発性の
  完了通知は右下トースト — 4秒で自動消滅)、
  選別セッションモードバー (UIUX-5: セッション中のみグリッド直上に表示。
  前へ / 採用 / 除外 / 次へ / 終了 + 進捗・件数。採用/除外は
  適用成功時のみ自動で次の画像へ進む)、
  仮想サムネイルグリッド、ステータスバー
- **右ペイン** (UIUX-5: セクションをアコーディオン化 — 既定展開はファイル
  情報/評価・選別/選別セッション (活性時)。折りたたみ状態は永続化):
  選択画像の情報 (パス・寸法・容量・形式・更新日時・DB状態・
  スキャン状態) + 写真メタデータ (WP-9: 撮影日時・カメラ・レンズ・GPS座標
  EXIF読み取り専用表示) + 生成メタデータ (WP-9: SD WebUI / ComfyUI /
  NovelAIのツール・モデル・Sampler・Steps・Seed・positive/negative prompt
  とComfyUI workflow JSONを折りたたみ表示・全文コピー。未知形式もRaw
  metadataとして表示・コピー可能) + AI実行履歴 (WP-10: AI由来タグ/
  キャプションには「AI」バッジ。手動編集済み情報は再生成で保護) +
  選別セッション (順序・位置・確認済み/採用/除外/保留カウント、途中終了→再開)

## AI支援 (WP-10)

OpenAI互換endpoint (llama.cpp等) への接続プロファイルを登録すると、
画像からのタグ・キャプション生成をバックグラウンドジョブとして実行できる。

- メニューバー「ツール>AIパネル…」→ プロファイル (Base URL・モデル・
  APIキー)・接続/テキスト/Vision/構造化JSONの4種テスト・ジョブキュー
  (一時停止/再開/失敗再試行)・登録ルートごとの自動処理設定
- APIキーはOS資格情報ストアへ保存 (利用不可環境では0600の平文ファイル+
  警告。DB・ログへは書かない)
- 送信プライバシー: EXIF/GPS送信はルート設定とプロファイル設定のAND
  (GPSは独立opt-in)。送信画像は最大辺1024pxへ縮小しメタデータを除去
- endpoint停止中もファイラー機能は継続し、ジョブはバックオフ
  (30s→2m→8m) で再試行。アプリ再起動後にジョブを再開
- 詳細な契約は [docs/dev/api-contract.md](docs/dev/api-contract.md) §44

## 自然言語検索・ハイブリッド検索 (WP-11/WP-12)

ローカルruri-v3埋め込みモデル (ONNX Runtime・独立ワーカープロセス) で
画像ごとの検索文書ベクトルを生成し、USearch HNSW索引で近傍検索する。

- AIパネル (「ツール>AIパネル…」) の「自然言語検索」タブで有効化
  (モデル選択・進捗・索引状態)
- 検索バーの重みスライダー「キーワード⇔意味」で、FTS5 bm25キーワード
  検索と意味検索の重み付きRRF融合 (w=0..1) を指定できる
- グリッドのコンテキストメニューや情報パネルの「類似画像を検索」で
  対象画像に意味的に近い画像を表示できる
- 埋め込み無効・索引未構築時もキーワード検索は常時機能し、意味検索は
  警告付きで省略される
- 索引は派生データ (`~/.local/share/gento-spike/indexes/`): 破損時は
  SQLiteのベクトル正本から自動再構築する
- 実測: 5万ベクトルで検索0.45ms・構築16.4s・索引144MiB
  ([docs/dev/measurements/wp12-usearch-50k.md](docs/dev/measurements/wp12-usearch-50k.md))
- 詳細な契約は [docs/dev/api-contract.md](docs/dev/api-contract.md) §45-46

キーボード: Space=クイックプレビュー、Enter/ダブルクリック=ビューア、
Esc=閉じる、F2=リネーム/WP-7、Delete=ゴミ箱/WP-7。右クリックで
コンテキストメニュー (OSのファイルマネージャーで表示 / 外部アプリで開く /
パスをコピー)。左ペインのフォルダー右クリックでルート登録/解除 (UIUX-1)。
ペイン幅・表示・サムネイルサイズ・スクロール速度 (UIUX-3: 既定2.0・
設定ダイアログ表示タブで変更)・左ペインタブ・検索行の折りたたみ (UIUX-3)・
情報パネルのセクション折りたたみ (UIUX-5)・スクロール位置はlocalStorageに
永続化され再起動後に復元される。

## ビューア (単一画像表示、WP-6 + UIUX-3)

- **画像送り (UIUX-3)**: 左右端の半透明‹ ›ボタン、←/→キー、**ホイール**
  (上=前の画像・下=次の画像。トラックパッドの慣性でも1ジェスチャ=1枚)。
  端ではそれ以上進まない (ボタンは無効表示)。
- **ズーム**: Alt+ホイール (ピンチ=Ctrl+ホイールもズーム)、+/−キー、
  0=全体表示 / 1=原寸 / r=リセット、ドラッグ=パン、ダブルクリック=
  全体⇔原寸切替。f=フルスクリーン。
- 高速選別モード (数字1-5=星評価・X=除外・P/A=採用、入力後自動で次へ)。
- 2/4枚比較ビューのホイールは**ズームのまま変更なし** (UIUX-3)。

## 登録ルート (永続ライブラリ、WP-2)

任意のディレクトリを登録ルートとして追加すると、初回スキャン → 差分スキャン →
BLAKE3による同一内容Asset統合 → フォルダ監視 (inotify) による外部変更の自動反映が
有効になる。登録ルートとファイル一覧はSQLite (`~/.local/share/gento-spike/library.sqlite3`)
へ永続化され、再起動後に復元される。詳細な契約は [docs/dev/api-contract.md](docs/dev/api-contract.md) §17〜21。

- **広域パス登録の確認 (UIUX-2)**: `/` や `/home` (POSIX depth≤1)・
  ドライブルート (`C:\`)・UNC共有ルート (`\\server\share`) を登録する場合は
  確認ダイアログが出る (誤操作でファイルシステム全体へ初回走査をかける事故の防止。
  `.`/`..` は先に字句解決してから判定 — `/home/..` も対象。
  キャンセルすれば登録されない。シンボリックリンク・マウントポイントは
  字面からは判定できないため、この確認はUX保護であってbackendの不変条件ではない)。
- **走査中のルート解除 (UIUX-2/3)**: 走査中にルートを解除すると、進行中の
  walkの列挙・DB書き込み・ハッシュが即座にキャンセルされる
  (`/` 規模の巨大ルートでも列挙tailを残さず停止する。他ルートの後続走査には
  影響しない)。

## 初回起動ガイド (WP-18)

登録ルートが0件の状態で起動すると、グリッド領域に3ステップの導線パネルを表示する
(ウィザードではなく空状態の充実。登録ルートを追加すると自動的に消える)。

1. **登録ルートを追加する** — 左ペイン「ライブラリ」タブの「登録ルート」に
   画像フォルダのパスを入力して追加するか、フォルダツリーでフォルダーを
   右クリックして「このフォルダーを登録ルートに追加」 (UIUX-1)。
   以降はライブラリ表示 (永続管理・監視) になる
2. **AI設定 (任意)** — メニューバー「ツール>AIパネル…」からOpenAI互換
   endpointのプロファイルを登録。
   AIなしでもファイラー機能 (閲覧・選別・タグ・検索・コレクション・ファイル操作) は
   すべて使用できる
3. **意味検索モデルの配置 (任意)** — 自然言語検索にはローカル埋め込みモデル
   (ruri-v3) のパッケージが必要。配置先は `--models-dir <PATH>` /
   環境変数 `GENTO_MODELS_DIR` / 既定のデータディレクトリ内 `models/`
   (AIパネルの「モデルの保存先」に表示)。パッケージの入手・変換手順は
   [docs/embedding-runtime.md](docs/embedding-runtime.md) §3〜4、変換の検証記録は
   [docs/dev/ruri3-onnx-verification.md](docs/dev/ruri3-onnx-verification.md) を参照

## 必要システムパッケージ (Ubuntu 24.04)

```bash
sudo apt install -y libwebkit2gtk-4.1-dev librsvg2-dev libxdo-dev libssl-dev libayatana-appindicator3-dev
```

詳細な環境記録は [docs/dev/environment.md](docs/dev/environment.md) を参照。

## セットアップ

Rust (rustc/cargo), Node.js, pnpm が必要。フロントエンド依存のインストール:

```bash
cd apps/desktop
pnpm install
```

## 開発起動

```bash
cd apps/desktop
pnpm tauri dev -- -- -- --dataset ../../../sample/dataset
```

- 引数は `tauri dev` の2番目の `--` 以降がアプリへ渡る
  (pnpmが1個、tauri CLIが1個の `--` を消費するため4個重ねる)
- `--dataset <PATH>` はアプリのCWD (`apps/desktop/src-tauri`) からの相対パスまたは絶対パス。
  省略時はCWD相対の `sample/dataset` を参照する
- `--cache-dir <PATH>` はサムネイルキャッシュのルート (WP-3)。
  省略時は `~/.cache/gento-spike/thumbnails` (XDG準拠)
- データセットが存在しなくても起動し、UIに「なし」と表示される
  (存在チェックの結果は起動ログ `dataset resolved` の `exists` でも確認できる)

## release build

```bash
cd apps/desktop
pnpm tauri build
```

生成物は `target/release/` と `target/release/bundle/` 以下。

### devtools feature (計測・検証専用機能の隔離)

計測ブリッジ (dev bridge)、障害注入 (`set_failure_injection`)、仮想5万件カタログは
cargo feature `devtools` で隔離されている (WP-0)。

- debug build (`debug_assertions`): 常に含まれる (開発・テストは従来どおり)
- 通常release build: **環境変数に関係なく存在しない** (invoke handlerに登録されない。
  `GENTO_DEV_BRIDGE=1` を与えても無効)
- 計測用release build: `cargo build --release -p gento-tauri --features custom-protocol,devtools`
  で明示的に有効化 (`custom-protocol` はtauri CLIがbuild時に自動で付けるfeatureで、
  cargo直接ビルドではfrontendDist埋め込みのために必要)。ビルド内では環境変数
  `GENTO_DEV_BRIDGE` (値は厳密に `1`) が追加のランタイムゲート

release buildの隔離と計測シナリオ動作は `scripts/release-smoke.sh` で検証する
(featureなしバイナリへのdev系識別子不在をバイナリレベルで確認)。

## 検証コマンド

Rust (リポジトリルートから):

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p gento-tauri --all-targets --features devtools -- -D warnings
cargo test --workspace
```

フロントエンド (`apps/desktop` から):

```bash
pnpm check   # svelte-check
pnpm test    # vitest
pnpm lint    # eslint
pnpm build   # vite build
```

## ログ制御

環境変数 `GENTO_LOG` でRust側ログレベルを制御する (tracing EnvFilter書式、既定 `info`、stdoutへ出力)。
形式はtracing-subscriber fmt既定の `TIMESTAMP LEVEL TARGET: message`。
panic時はpanic情報 + バージョン + OS + APIバージョンを
`~/.cache/gento-spike/log/panic.log` へ追記する。

```bash
GENTO_LOG=debug pnpm tauri dev -- -- -- --dataset ../../../sample/dataset
```

## アプリケーションディレクトリ (WP-0 / WP-18-1)

| 用途 | 既定位置 (Linux) | 寿命・バックアップ |
|---|---|---|
| ライブラリDB (正本) | `~/.local/share/gento-spike/library.sqlite3` | migration前に `db-backups/` へ自動backup (3世代) |
| アプリ設定 (`app_settings`) | DB内 (`library.sqlite3`) | DBと運命を共にする (上のbackupに含まれる) |
| migration・フルDB backup | `~/.local/share/gento-spike/db-backups/` | `library-pre-*` (migration用) と `library-full-*` (手動/自動) |
| ポータブルバックアップ (`.gento`) | 作成時に指定したdir (既定は `db-backups/`) | 評価・タグ・キャプション・コレクション+ファイル照合情報 |
| サムネイルキャッシュ | `~/.cache/gento-spike/thumbnails` | **削除しても再生成可能** (元画像・DBから自動復元) |
| ベクトルindex (意味検索) | `~/.local/share/gento-spike/indexes/` | **削除・破損してもDBから自動再構築** |
| 埋め込みモデル | `--models-dir <PATH>` / `GENTO_MODELS_DIR` / `~/.local/share/gento-spike/models/` | 再ダウンロード可能 (AIパネルの「モデルの保存先」に表示) |
| ログ・panic記録 | `~/.cache/gento-spike/log` | 診断用 (削除可) |

- DBパスは `--db <PATH>` / 環境変数 `GENTO_DB` で上書き可能 (テスト・計測用)。
  `--cache-dir <PATH>` はサムネイルキャッシュルートのみを上書きする (計測ドライバ互換)。
- **アンインストール時のデータ保持**: アプリ (deb/rpm/AppImage) を削除しても
  `~/.local/share/gento-spike/` (DB・設定・登録ルート情報・バックアップ) は
  **削除されない** (OSのパッケージ削除はユーザーデータを触らない慣行どおり。
  アンインストール前に `.gento` ポータブルバックアップを作成しておくと
  別環境への引継ぎも可能 — 復元演習は `scripts/wp18_backup_restore_drill.sh`)。
- リリース前の検証手順 (データ保持・復元を含む全ゲート) は
  [docs/dev/release-checklist.md](docs/dev/release-checklist.md)。

## 依存ライセンス一覧

`docs/dev/licenses.md` (自動生成)。再生成: `scripts/gen-licenses.sh` (要 cargo / jq / pnpm)。
依存を追加・更新した際は再生成してコミットすること。

## 計測 (WP-7 / フェーズ9)

release buildでの計測手順 (詳細は [docs/technical-spike-results.md](docs/technical-spike-results.md)、
ドライバとログは `docs/dev/measurements/` を参照):

```bash
cd apps/desktop
pnpm build   # フロントエンドdist (cargoへ埋め込まれる)
cd ../..
cargo build --release -p gento-tauri --features custom-protocol,devtools   # 計測用release build

GENTO_LOG=info GENTO_DEV_BRIDGE=1 NO_COLOR=1 \
  ./target/release/gento-tauri --dataset ./sample/dataset \
  --cache-dir /tmp/gento-cache 2>&1 | tee app.log
```

(tauri CLI経由で行う場合は `pnpm tauri build --no-bundle -- --features devtools`)

- **計測用release buildには `--features devtools` が必須** (WP-0)。通常release
  buildには計測コマンドが存在しない。cargo直接ビルドでは frontendDist 埋め込みの
  `custom-protocol` も併せて指定すること (tauri CLI経由なら自動)
- `GENTO_DEV_BRIDGE=1` (値は厳密に `1`) で計測ブリッジ (F2〜F12、ctrl+F1=UI状態リセット) とバックエンド計測コマンドが有効になる。dev buildでもこの環境変数が必要で、未指定・`0`・その他の値では無効
- アプリ内開発パネル (メニュー「ツール>開発パネル」) に 統計 (優先度別キューを含む) / long task・ロングフレーム / 起動タイムマーク / エラー注入 を表示。UIUX-3でグリッド統計 (DOMセル数・可視row・需要) も開発パネル側へ移設 — 主UIのオーバーレイ表示は廃止し、パネルを開いている間だけ400ms間隔で取得する
- ブリッジは1秒周期の機械可読行を `devtest:` プレフィックスでログ出力する (`grep devtest:` で採取。G=グリッド、T=サムネイル、V=ビューア、C=比較、P=パフォーマンス、U=UIシェル状態)。UIUX-3でvwCmdMode (素F12→n/p=wheel送り・i/o=Altズーム・c/d=pinch・b/f=送りボタン) を追加
- Rustプロセス+WebView子プロセスのRSS合計は `/proc` を読むスクリプトで測定 (例: `rss-sample.sh <pid>`)
- SIGTERMでgraceful終了 (ワーカー排出・一時ファイル掃除)。終了後 `find <cache-dir> -name '.tmp-*'` が空であることを確認できる

## releaseスモーク (WP-0受け入れ証拠)

```bash
scripts/release-smoke.sh
```

(a) featureなしreleaseバイナリからdev系識別子が不在であること、(b) `--features devtools`
buildで計測シナリオ (起動→スキャン→サムネイル生成→graceful終了) が動作すること、
(c) データセットなしで起動し空のUIを表示すること、を検証する
(Xディスプレイとxdotoolが必要、既定 `DISPLAY=:1`)。
