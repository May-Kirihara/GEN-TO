# GEN-TO

高速画像表示の技術検証プロトタイプ。Tauri 2 + Svelte 5 + TypeScript + Rust (Cargo workspace) で構成する。
本リポジトリは製品MVPではなく、[技術検証計画](docs/technical-spike.md) に基づくスパイク実装。

## リポジトリ構成

- `crates/gento-core` — ID・基本ドメイン型 (Tauri非依存)
- `crates/gento-image` — 画像パイプライン (走査・プローブ・サムネイル・キャッシュ、Phase 2/3で実装。Tauri非依存)
- `apps/desktop` — Svelte 5 + Vite フロントエンドと `src-tauri` (Tauri 2 thin adapter)

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

## 検証コマンド

Rust (リポジトリルートから):

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
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

```bash
GENTO_LOG=debug pnpm tauri dev -- -- -- --dataset ../../../sample/dataset
```

## 計測 (WP-7 / フェーズ9)

release buildでの計測手順 (詳細は [docs/technical-spike-results.md](docs/technical-spike-results.md)、
ドライバとログは `docs/dev/measurements/` を参照):

```bash
cd apps/desktop
pnpm tauri build --no-bundle   # frontendDistビルド + cargo --release
GENTO_LOG=info GENTO_DEV_BRIDGE=1 NO_COLOR=1 \
  ../../target/release/gento-tauri --dataset ../../sample/dataset \
  --cache-dir /tmp/gento-cache 2>&1 | tee app.log
```

- `GENTO_DEV_BRIDGE=1` (値は厳密に `1`) で計測ブリッジ (F2〜F12) とバックエンド計測コマンドが有効になる。dev buildでもこの環境変数が必要で、未指定・`0`・その他の値では無効
- アプリ内開発パネルに 統計 (優先度別キューを含む) / long task・ロングフレーム / 起動タイムマーク / エラー注入 を表示
- ブリッジは1秒周期の機械可読行を `devtest:` プレフィックスでログ出力する (`grep devtest:` で採取)
- Rustプロセス+WebView子プロセスのRSS合計は `/proc` を読むスクリプトで測定 (例: `rss-sample.sh <pid>`)
- SIGTERMでgraceful終了 (ワーカー排出・一時ファイル掃除)。終了後 `find <cache-dir> -name '.tmp-*'` が空であることを確認できる
