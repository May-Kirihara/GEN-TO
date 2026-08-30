# GEN-TO

高速画像表示の技術検証プロトタイプ。Tauri 2 + Svelte 5 + TypeScript + Rust (Cargo workspace) で構成する。
スパイク ([技術検証計画](docs/technical-spike.md)、[結果報告](docs/technical-spike-report.md)、[計測値](docs/technical-spike-results.md)) を母体に、MVP実装 ([MVP実装計画](docs/mvp-implementation-plan.md)) を進行中。

## リポジトリ構成

- `crates/gento-core` — ID・基本ドメイン型・repository抽象・登録ルート走査エンジン (Tauri非依存)
- `crates/gento-db` — SQLite基盤 (migration・repository実装。メタデータの正本)
- `crates/gento-image` — 画像パイプライン (走査・プローブ・BLAKE3・サムネイル・キャッシュ。Tauri非依存)
- `apps/desktop` — Svelte 5 + Vite フロントエンドと `src-tauri` (Tauri 2 thin adapter)

## 製品UIシェル (3ペイン、WP-4)

アプリは3ペイン構成の画像ファイラーとして動作する:

- **左ペイン**: フォルダツリー (ホーム/ファイルシステム起点の遅延ロード。
  未登録フォルダも閲覧可能。登録ルートはバッジ表示) + 登録ルート管理 +
  コレクション/保存済み検索のプレースホルダ (WP-6/8)
- **中央ペイン**: パンくず + ソート + ビューア種別 + サムネイルサイズの
  ツールバー、仮想サムネイルグリッド、ステータスバー
- **右ペイン**: 選択画像の情報 (パス・寸法・容量・形式・更新日時・DB状態・
  スキャン状態) + 選別セッション (順序・位置・確認済み/採用/除外/保留カウント、
  途中終了→再開)

キーボード: Space=クイックプレビュー、Enter/ダブルクリック=ビューア、
Esc=閉じる、F2=リネーム/WP-7、Delete=ゴミ箱/WP-7。右クリックで
コンテキストメニュー (OSのファイルマネージャーで表示 / 外部アプリで開く /
パスをコピー)。ペイン幅・表示・サムネイルサイズ・スクロール位置は
localStorageに永続化され再起動後に復元される。

## 登録ルート (永続ライブラリ、WP-2)

任意のディレクトリを登録ルートとして追加すると、初回スキャン → 差分スキャン →
BLAKE3による同一内容Asset統合 → フォルダ監視 (inotify) による外部変更の自動反映が
有効になる。登録ルートとファイル一覧はSQLite (`~/.local/share/gento-spike/library.sqlite3`)
へ永続化され、再起動後に復元される。詳細な契約は [docs/dev/api-contract.md](docs/dev/api-contract.md) §17〜21。

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

## アプリケーションディレクトリ (WP-0)

| 用途 | 既定位置 |
|---|---|
| データ (MVP-WP-1のSQLite DB予定) | `dirs::data_dir()/gento-spike` (Linux: `~/.local/share/gento-spike`) |
| キャッシュ | `dirs::cache_dir()/gento-spike` (Linux: `~/.cache/gento-spike`) |
| ログ・panic記録 | `dirs::cache_dir()/gento-spike/log` |

`--cache-dir <PATH>` はサムネイルキャッシュルートのみを上書きする (計測ドライバ互換)。

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
- アプリ内開発パネル (ヘッダー右の「開発パネル」) に 統計 (優先度別キューを含む) / long task・ロングフレーム / 起動タイムマーク / エラー注入 を表示
- ブリッジは1秒周期の機械可読行を `devtest:` プレフィックスでログ出力する (`grep devtest:` で採取。G=グリッド、T=サムネイル、V=ビューア、C=比較、P=パフォーマンス、U=UIシェル状態)
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
