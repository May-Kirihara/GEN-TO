# 開発環境記録 (Phase 0)

- 記録日: 2026-08-28 (実測値)

## OS / ハードウェア

- OS: Ubuntu 24.04 (noble), kernel 7.0.0 x86_64
- CPU: Intel Core i5-13500 (20論理CPU)
- RAM: 94GiB
- GPU: NVIDIA GeForce RTX 3090 Ti
- ディスク: /mnt/holyland 616GiB 空き
- ディスプレイ: X11 `DISPLAY=:1`, 3424x3852 @144dpi (高DPI。計測記録にはDPI要因を明記すること)

## ツールチェーン

| ツール | バージョン |
|---|---|
| rustc / cargo | 1.96.0 (ac68faa20 2026-05-25) |
| Node.js | v25.8.2 (非LTS。問題時はLTS 22系でも検証する方針: PM計画 §7-11) |
| pnpm | 10.29.2 (`packageManager` フィールドで固定) |
| npm | 11.11.1 (参考。パッケージ操作にはpnpmを使う) |
| cargo-tauri | 導入しない。`apps/desktop` の devDependency `@tauri-apps/cli` (2.11.4) を使い `pnpm tauri` で実行 (`pnpm dlx`/`npx` はバージョン漂動を避けるため不使用) |

## Tauri前提システムライブラリ (pkg-config実測)

| パッケージ | バージョン |
|---|---|
| webkit2gtk-4.1 | 2.52.3 (devパッケージ導入済み) |
| gtk+-3.0 | 3.24.41 |
| glib-2.0 | 2.80.0 |
| gdk-pixbuf-2.0 | 2.42.10 |
| openssl | 3.0.13 |

- `libxdo-dev` 導入済み。Ubuntuのlibxdo-devは`.pc`ファイルを持たず、ヘッダ+`.so`のみで正常 (Tauriビルドに影響なし)
- 導入コマンド (Ubuntu 24.04):
  ```bash
  sudo apt install -y libwebkit2gtk-4.1-dev librsvg2-dev libxdo-dev libssl-dev libayatana-appindicator3-dev
  ```

## データセット

- `sample/dataset`: PNG 1,615件・約957MiB (docs/implementation-plan.md §4)
- 取り扱い: 読み取り専限。`.gitignore` で管理対象外。不存在環境でも起動・テストが失敗しない

## ネットワーク

- registry.npmjs.org / static.crates.io 到達可能
