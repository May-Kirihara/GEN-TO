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
