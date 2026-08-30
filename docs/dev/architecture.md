# アーキテクチャ (WP-0時点の実装に即した要約)

- 作成日: 2026-08-30 (WP-0)
- 目的: crate責務・依存方向・公開APIの要点・devtools featureの境界を
  実装に即して1枚で把握するためのもの。詳細な契約は
  [api-contract.md](api-contract.md) を参照。

## 依存方向

```
gento-core  (ドメイン型。Tauri非依存)
    ↑
gento-image (画像パイプライン。Tauri非依存。gento-coreに依存)
    ↑
gento-tauri (Tauri thin adapter。gento-core + gento-imageに依存)
```

Tauriに依存してよいのは `gento-tauri` のみ。逆方向の依存
(gento-core/image → tauri) は禁止。

## 各crateの責務と公開APIの要点

### `crates/gento-core` — ドメイン型・カタログ

- `AssetId` (u64新type。IPCでは10進文字列化してJS安全整数問題を回避)
- `HealthInfo` (`health` commandのDTO。`api_version` 含む)
- `GENTO_API_VERSION` 定数と `AppDirs` (data/cache/logディレクトリの
  解決結果を保持するだけの構造。解決はアダプター側。MVP-WP-1のSQLite DBは
  `data_dir` 直下へ置く予定)
- `jobs`: `SizeClass` / `Priority` (ジョブ系の基本型)
- `catalog`: `ImageRecord`・`CatalogState` (安定順序・generation付き
  ページング・上限500件強制)・ページDTOとエラー形状
- `catalog::virtual_catalog` (**devtools**): 5万件仮想カタログ生成

### `crates/gento-image` — 画像パイプライン (Tauri非依存)

- `scan`: walkdirによるPNG再帰走査 (エラー隔離・バッチsink)
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
- `catalog.rs`: カタログcommand群 (summary/page/scan) と走査スレッド。
  ページDTOへのversion付与もここ (thin adapter層の責務)
- `thumbnails.rs`: サムネイル生成サービス (要求の受付・dedup・イベント
  バッチング・統計DTO) とcommand群
- `protocol.rs`: ローカル画像プロトコル3種 (gento-thumb/preview/original)。
  URL構造化パース・厳密検証・TOCTOU閉鎖・統計
- `devtools.rs` (**devtoolsのみ**): 計測ブリッジcommand
  (`dev_bridge_enabled` / `devtest_report`) と環境変数ゲート

フロントエンド (`apps/desktop/src`) はSvelte 5 + TS。ストア層
(catalog/thumbnails/grid/viewer/compare) はIPCを注入されたプレーンな
クラスで、Svelteコンポーネントから利用する。

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
| `data_dir` | `~/.local/share/gento-spike` | MVP-WP-1のSQLite DBはここへ |
| `cache_dir` | `~/.cache/gento-spike` | サムネイルは `cache_dir/thumbnails` |
| `log_dir` | `~/.cache/gento-spike/log` | `panic.log` |

`--cache-dir <PATH>` 起動引数はサムネイルルートのみ上書きする
(計測ドライバ互換。data/log位置は動かない)。

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
