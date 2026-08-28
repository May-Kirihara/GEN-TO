# GEN-TO 技術検証プロトタイプ 実装 PM計画

- 作成日: 2026-08-28（rev.2 — Codex計画レビュー1回目の指摘を反映）
- PMエージェント作成。実装本体は `docs/implementation-plan.md`（以下「本体計画」）に従う。
- リスク分類: **normal** — 新規グリーンフィールドであり既存コード破壊リスクは低いが、多岐にわたる新規ファイル・依存追加・並行処理を含むため標準ゲートを全程適用する。SQLite・AI・タグ等の high-risk 要素は本体計画 §18 で明示的に非対象。

## 1. 目標とスコープ

本体計画のフェーズ0〜9を実装し、Linux開発機で技術検証プロトタイプを成立させる。
完了定義は本体計画 §21 のチェックリスト。Phase 9 の手動GUI計測は、スクリプト化可能な範囲をWP-7で実行し、手動限定項目（体感・フレーム落ち等）はユーザー確認用チェックリストとして結果文書に明示する。

非対象: 本体計画 §18（SQLite、AI、タグ、ファイル操作、JPEG等、パッケージ配布ほか）。

## 2. Phase 0: 環境記録（実測 2026-08-28）

- OS: Ubuntu 24.04 (noble), kernel 7.0.0 x86_64
- Rust: rustc/cargo 1.96.0
- Node.js v25.8.2, npm 11.11.1, pnpm 10.29.2
- Tauri CLI: cargo-tauri なし → `apps/desktop` の devDependency `@tauri-apps/cli` を使用。実行は `pnpm tauri`（`pnpm dlx`/`npx` はバージョン漂移を避けるため不使用）
- WebKitGTK: runtime 2.52.3導入済み。**dev パッケージ未導入**（後述ブロッカー）
- GTK 3.24.41, glib 2.80.0, gdk-pixbuf 2.42.10 導入済み
- ディスプレイ: X11 :1, 3424x3852 @144dpi（高DPI。計測時はDPI要因を記録）
- ネットワーク: registry.npmjs.org / static.crates.io 到達可能
- ディスク: /mnt/holyland 616GiB 空き
- `sample/dataset`: 1,615 PNG 確認済み

### ブロッカー: システム開発パッケージ不足

`libwebkit2gtk-4.1-dev` `librsvg2-dev` `libxdo-dev` `libssl-dev` `libayatana-appindicator3-dev` が未導入。
Tauri 2 のRustビルド（`webkit2gtk-sys` 等のpkg-config）に必須のため、導入なしではアプリビルド・起動・release buildが不可能。
PM権限では sudo 実行不可のため、**ユーザーによる apt install を依頼する**（本体計画 §3の「不足時はパッケージと理由を報告」に従う）。

必要コマンド:
```bash
sudo apt install -y libwebkit2gtk-4.1-dev librsvg2-dev libxdo-dev libssl-dev libayatana-appindicator3-dev
```

導入される主要コンポーネント: webkit2gtk-4.1 / javascriptcoregtk-4.1 / libsoup-3.0 / librsvg2 / xdo / openssl の各ヘッダと .pc。いずれも Tauri 公式 Linux 前提パッケージ。

任意（Phase 9 のGUI操作自動化用）:
```bash
sudo apt install -y xdotool
```

## 3. 依存ライブラリ選定方針（初期決定、詳細はWP-1で docs/dev/dependency-notes.md にメモ化）

### Rust
- `image`（0.25系）: デコード・リサイズ。**default-features を絞り**（PNG復号のみ有効化）しコンパイル時間と攻撃面を削減。
- `png`: IHDRのみ読む寸法プローブ（全文デコード回避、本体計画 §9-3）。ヘッダ情報取得APIは実装時に docs.rs の当該バージョンで確認する（レビューで指摘された個別メソッド名は鵜呑みにしない）。デコード制限（decompression limit等）を設定する。
- `webp` crate: lossy WebP エンコード（libwebp をcc/build-essentialでビルド、BSD-3-Clause）。**バージョンを pin** し、`image` feature 結合が曖昧なら明示的 RGB/RGBA バッファを渡す。
  - フォールバックは「無条件切替」ではなく**計測ゲート付き**: lossless WebP はサイズ/レイテンシ特性が大きく変わるため比較計測を必須とし、JPEG は透過が失われるため透過画像には不採用。結果文書に根拠を残す（本体計画 §11-45）。
- `walkdir`: 走査。`serde`/`serde_json`: DTO。`tracing` + `tracing-subscriber`: 開発用ログ。
- `twox-hash`: キャッシュキーの高速ハッシュ（非暗号）。衝突時の誤供給を防ぐため、キー文字列自体（ハッシュ前）をファイル名の一部に含める。将来のAsset内容ハッシュへ差し替え可能なAPI設計（本体計画 §11）。
- `dirs`: キャッシュディレクトリ規定位置解決。
- ワーカープールは **自作**（std thread + Mutex<BinaryHeap> + Condvar、上限付き・優先度付き・協調キャンセル）。理由: 要件は「優先度付き・上限付き・再優先度化・キャンセル・重複統合」であり、rayon はスレッド数は有界だが優先度キュー・入場制限・キャンセル・dedup通知のセマンティクスを提供しないため。自作は並行処理コンポーネントとして重要度が高く、専用テスト群（§5）を課す。設計要件: 再優先度化、停滞ヒープエントリの無効化、キュー満杯時の入場制御、優先度逆転対策、ワーカーpanic隔離、キャンセルトークン、dedup待機者への通知、シャットダウン、一時ファイル後始末。

### TypeScript / Svelte
- Svelte 5 + Vite + TypeScript (strict)。
- **TanStack Virtual は公式 Svelte アダプタ `@tanstack/svelte-virtual` を第一候補で検証**（本体計画の第一候補に合致）。WP-5 の初手に Svelte 5 (runes) との互換性プローブを置き、不適合なら `@tanstack/virtual-core` 直接使用へ切り替える（本体計画 §10 判断ゲートAの選択肢2前倒し）。さらに問題あれば選択肢3（命令的DOM更新）。プローブ結果は依存メモに記録。
- `vitest` + `@testing-library/svelte`（可能範囲）: フロント単体テスト。
- `eslint` (flat config) + `eslint-plugin-svelte` + `typescript-eslint`、`svelte-check`。`prettier`は導入しない。
- パッケージマネージャ: pnpm（apps/desktop 単一パッケージ、`packageManager` フィールドでバージョン固定）。
- `@tauri-apps/cli` / `@tauri-apps/api` / Rust `tauri` のメジャーバージョン互換（すべて v2系）を確認して pin。

## 4. Work packages（委任順）

各WPは実装者 (implementer subagent) へ委任。原則シリアル実行（DTO/API形状が前WPで確定するため）。テストファーストを指示。
WP-3/WP-4 で凍結する DTO・イベント契約は、WP-2 終了時に API契約チェックポイントとして文書化し、後続WPとの不整合を防ぐ。

| WP | 本体計画 | 内容 | 主要成果物 |
|---|---|---|---|
| WP-1 | Phase 0+1 | workspace スキャフォールド、Tauri+Svelte最小アプリ、health command、lint/test設定、`.gitignore`、README、環境・依存メモ | Cargo workspace, crates/*, apps/desktop, docs/dev/*.md |
| WP-2 | Phase 2 | PNG走査・寸法プローブ・カタログ・ページングcommand・仮想5万件カタログ・進捗/エラーイベント・API契約メモ | gento-core, gento-image(走査系), tests, docs/dev/api-contract.md |
| WP-3 | Phase 4 | サムネイル/preview生成、WebPキャッシュ、キャッシュキー、分散ディレクトリ、アトミック配置、上限付き優先度ワーカープール（再優先度化・キャンセル・dedup・一時停止/再開 API を含む）、**2GiB上限の会計と退避（アクセス時刻LRU）**、統計 | gento-image(生成系), tests |
| WP-4 | Phase 5 | `gento-thumb://` `gento-preview://` **および `gento-original://`（原寸表示用・登録済みカタログ資産のみに解決）** の各プロトコル、§4.1セキュリティ契約の実装とテスト、未生成時のプレースホルダーフロー、CSP設定 | src-tauri protocol, tests, tauri.conf.json |
| WP-5 | Phase 3 | TanStack Svelteアダプタ互換プローブ→仮想グリッド、選択3種、キーボード移動、スクロール位置保存復元、開発パネル | apps/desktop/src, tests |
| WP-6 | Phase 6+7 | 単一ビューア（fit/actual/custom、**preview→original の段階的読み込み**、マウス中心ズーム、パン、先読み、世代ID破棄）、2/4枚比較（同期ズーム・パン、同期解除、基準スロット、スロット昇格） | apps/desktop/src, tests |
| WP-7 | Phase 8+9 | 優先度更新・キャンセル・耐障害（エラー注入、シャットダウン、panic隔離）、**E2E統合フローの名前付き検証（可視行/ビューア→需要バッチ→優先度更新→dedup→生成→完了イベント→generation付きURL更新→停滞需要のキャンセル）**、一時停止/再開UI、再起動時キャッシュ再利用、release buildによるスクリプト化計測、`docs/technical-spike-results.md`（暫定判断+手動確認チェックリスト）、handoff | 全領域, docs |

各WPの受け入れ条件は本体計画各フェーズの「受け入れ条件」節をそのまま適用する。

### 4.1 WP-4 ローカルプロトコルのセキュリティ契約

- URLのどの部分もファイルシステムパスとして解釈しない。解決は**アプリ保有のレジストリ（ID→登録資産/キャッシュ）経由のみ**。
- 厳密な検証: scheme・authority/host（Linux では `scheme://localhost/...` 形式で届く可能性を含む）・HTTPメソッド・セグメント数・ID構文・サイズクラスenum・クエリ（cache generation）。過剰セグメント・不正ID・未知クエリは拒否。
- パーセントエンコード分離子・二重デコード・バックスラッシュ・NUL・`..` を含む一切の入力を拒否するテスト。
- キャッシュルートの正規化・包含チェック、シンボリックリンク追従の禁止（検証とopen間のTOCTOUを避ける設計）。
- 応答は正規ファイルのみ・サイズ上限あり。`Content-Type` 正確、`X-Content-Type-Options: nosniff`、意図的なキャッシュヘッダ。エラー本文・ログにパスを漏らさない。
- CSP は必要な画像schemeのみ許可。
- プロトコルハンドラは**非同期**でファイル読み込みし、UI/プロトコルスレッドをブロックしない。
- dev と release でカスタムプロトコル挙動が一致することを確認する。

## 5. テスト戦略

- Rust: `cargo test` 各crate単位。本体計画 §17 Rust単体テスト項目に加え:
  - キャッシュ削除→再生成、再起動後キャッシュ再利用
  - キャッシュキー衝突耐性（キー文字列のファイル名埋め込み検証）
  - キュー満杯/バックプレッシャー、再優先度化・キャンセル競合、ワーカーpanic隔離、シャットダウン・一時停止/再開
  - 一時ファイルのクロスファイルシステム rename 問題（一時ファイルを最終配置先と同一ディレクトリへ作る設計の検証）
  - 退避（eviction）と生成中ジョブ・プロトコル読み取りの競合
  - 破損/切り詰めPNG、decompression bomb対策の制限、異常寸法
  - 透過保持・lossy出力の妥当性検証
  - プロトコル: メソッド/host/クエリ/セグメントの拒否、cache generation 挙動
- フロントエンド: vitest。§17 項目（選択集合、Shift範囲、キーボード移動、列数、ViewTransform、比較同期計算、古い応答破棄）に加え、高速な選択変更の統合テスト、ウィンドウリサイズ（列数再計算）。
- 統合: 需要→表示までのE2Eフロー（WP-7）、Rustカタログ→ページ取得、プロトコル許可・拒否。
- プロトコル↔WebView 境界は自動テストに加え、release buildでの再現可能な手動テスト手順を結果文書に残す。
- データセット不存在環境でも全テストが通ること（fixtureはテスト用一時ディレクトリ生成、`sample/dataset` には依存しない）。

## 6. ドキュメント・言語影響

- 日本語ドキュメント: README（実行手順）、docs/dev/環境メモ・依存メモ・API契約メモ、docs/technical-spike-results.md、handoff (docs/handoff/)。
- コメント・識別子は英語可、説明文は日本語を基本（本体規約）。

## 7. リスク

1. **システムパッケージ未導入** — Phase 1+のビルド・起動検証が不可能。ユーザー依頼済み。
2. **`webp` crate のCビルド失敗** — 計測ゲート付きフォールバック（§3）。
3. **Svelte 5 + TanStack アダプタ互換性** — プーブ→core直使用→命令的DOMの段階対処（判断ゲートA）。
4. **スコープ超過** — 7 WPで分割し各WP検証後に前進。本体計画 §18 外はTODO記録のみ。
5. **WebViewキャッシュ** — URLへcache generationを含め対策（本体計画 §12）。
6. **並行PNGデコード+RGBA変換のメモリスパイク**（preview/比較負荷）— 同時デコード数をプール上限で制御し、RSSを計測。
7. **非暗号ハッシュのキー衝突・mtime不変の陳腐化** — キー文字列埋め込み、将来の内容ハッシュ移行API。
8. **イベント嵐・WebView同時リクエスト上限**（高速スクロール）— 通知のバッチ化・要求合并、実測で確認。
9. **dev/release のプロトコル挙動差** — WP-4で両モード確認。
10. **高DPI(144dpi)・強力な開発機が低スペック環境の問題を隠す** — 計測記録にハードウェア・DPIを明記し、過剰並列化しない。
11. **Node 25 が非LTS** — 問題発生時はLTS (22系) でも検証。libwebp はBSD-3-Clause、サプライチェーンはバージョンpinで固定。

## 8. 仮定台帳

| # | 仮定 | 根拠 | 反証条件 | 破壊半径 |
|---|---|---|---|---|
| A1 | ユーザーがapt installを実行/承認する | apt候補あり、本体計画§3 | **解決済み** (2026-08-28: 導入確認。libxdoはUbuntuでは.pc無し・ヘッダ+.soのみで正常) | — |
| A2 | `webp` crateが本環境でビルド可能 | build-essential導入済み、cc利用可 | cargo build失敗 → 計測ゲート付きフォールバック | サムネイル形式のみ |
| A3 | TanStack SvelteアダプタがSvelte 5で機能、不然はvirtual-core直使用で成立 | 判断ゲートA選択肢1→2 | 両者不合格 → 選択肢3 | WP-5 |
| A4 | IPC DTOのIDは文字列 | 本体計画 §9 優先案 | 特になし（既に計画決定） | — |
| A5 | Phase 9 の体感・フレーム落ち等は手動確認となる。自動化可能分（起動時間・走査・生成p50/95/99・プロトコル応答・RSS・キュー長）はスクリプト計測する | 性質上 | — | 結果文書の判断は「暫定」となる |
| A6 | git は **WP単位でコミットを作成**（ユーザー指示 (b)、2026-08-28） | ユーザー回答 | — | 履歴管理のみ |
| A7 | `sample/dataset` は読み取り専用として扱う | 本体計画 §4.1 | — | — |
| A8 | Node 25 + Vite/Svelte 5 最新安定の互換 | 両者アクティブ開発中 | pnpm install/build失敗 → バージョン固定で解消 | WP-1 |
| A9 | xdotool導入時、release buildでのGUI操作をある程度スクリプト化できる | xdotool候補あり | **導入済み**。操作不安定な場合のみ該当シナリオを手動チェックリストへ | Phase 9自動化範囲 |

## 9. ゲート適用と履歴

- GaOTTT recall: ツール未接続のため**利用不可**（最終報告に明記）。
- 計画レビュー: Codex CLI 1回目 **reject**（ブロッカー4件: 原寸配信経路未定義、Phase 9不完全、2GiB上限と一時停止の欠落、E2E統合の不在）→ 本rev.2で反映。2回目 **approve-with-notes**（ブロッカーなし）。残る非ブロッカー3件は委任契約へ織り込み済み:
  1. 2GiB不変条件の厳密定義（一時ファイル・生成中ファイルの算入、退避トリガ）→ WP-3契約
  2. Phase 9の自動/手動実施区別の記録、未実施手動確認からのPASS推論禁止 → WP-7契約
  3. キャッシュキーのファイル名埋め込みは安全にエンコードし長さ制限 → WP-3契約
- QA計画レビュー: ⭕スキップ（本体計画が詳細な受け入れ条件を既に定義、rev.2でテスト行列を拡充）。
- テストファースト委任 ✅ / PM差分検査 各WP ✅ / Codex最終diffレビュー ✅ / QA最終レビュー ✅。
