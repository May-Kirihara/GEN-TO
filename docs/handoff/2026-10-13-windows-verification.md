# Windows実機検証記録 (WIN-1 / WP-W3)

- 作成: 2026-10-13 (PMエージェント。ユーザー実機検証の進行に合わせて更新)
- 計画: [docs/plans/2026-10-13-windows-support-plan-pm.md](../plans/2026-10-13-windows-support-plan-pm.md)
- チェックリスト正本: platform-notes §8 (8.2 主要フロー12項 + 8.3 Windows特有5項)
- 結果の記法: **実施 / 合格 / 不合格 / 既知制限として受理** の4値+証拠

## 1. ステータス

- **WP-W2 (最初のビルド): 完了** — 2026-10-13、Windows実機で
  `pnpm tauri build` 成功、インストール、起動、グリッドUI描画、
  フォルダツリー展開・ナビゲーション動作を確認。
- **意味検索 (embedding・索引・モード選択): target\release直起動で動作確認済み**
  (2026-10-13。ワーカーEXE_SUFFIX修正後。インストール版はワーカー同梱待ち=キュー#11)
- WP-W3 (§8チェックリスト): 進行中 (§4の表)
- 手順の正本: [docs/dev/windows-build.md](../dev/windows-build.md) (2026-10-13実績反映済み)

## 2. 環境証拠 (ユーザー報告 + セッションlog)

| 項目 | 値 |
|---|---|
| OS | Windows 11 (詳細バージョン未記録 — `winver` で後日補記) |
| IDE/コンパイラ | Visual Studio 18 Community / MSVC 14.51.36231 (cl 19.51) |
| NASM | 3.02 (手動PATH追加) |
| meson / ninja | 1.12.1 / (バージョン未記録) |
| pkg-config | 導入済み (種別未記録 — choco/MSYS2 後日補記) |
| Rust/Cargo | (未記録 — `rustc -vV` 後日補記) |
| numkong | 7.8.5 (Windows側ローカルlock更新。正規repoは7.8.2のまま — 要指示) |
| dav1d | 1.5.0 を dav1d-sys 0.8.3 内蔵のソースビルドで静的リンク (Route B) |

### ビルド時の必須環境変数 (日本語Windows)

```bat
set SYSTEM_DEPS_DAV1D_BUILD_INTERNAL=auto
set CFLAGS=/utf-8
set CXXFLAGS=/utf-8 -DNK_DYNAMIC=
```

詳細と原因は windows-build.md §2/§6 を参照。

## 3. ビルド過程で発見・修正した事項 (時系列)

| # | 事象 | 解決 |
|---|------|------|
| 1 | dav1d-sys: pkg-config不在panic | Route B (ソースビルド) へ切替 + pkg-config導入 |
| 2 | dav1d meson: `Program 'nasm' not found` | NASM 3.02導入 + PATH追加 |
| 3 | numkong 7.8.2/7.8.5: 全dispatch TUがexit 2 (cargoログに実エラー無し) | 手動cl再現で `C2059/C2065/C2099` を特定 → **CP932誤読** → `CFLAGS=/utf-8` |
| 4 | usearch: 同様のexit 2 (C++ TU) | `CXXFLAGS=/utf-8` (CFLAGSはCにのみ効く) |
| 5 | リンク: `LNK2019 __imp_nk_*` ×5 | numkong 7.8.5のdllimport設計 vs 静的リンク → `CXXFLAGS` に `-DNK_DYNAMIC=` 追加 |
| 6 | gento-tauri自体: unix専用コードのE0432/E0433/E0599/E0061/E0716 ×10 | **WP-WIN-PORT-1** (implementer): 3ファイルをcfg-gate + `os_file_id_of` 規約統合。Linux全ゲート緑 (test 976) |
| 7 | フォルダツリーが展開できない (と思われた) | **誤解**: 展開矢印クリックで正常動作。コンソールエラー無し。ラベルクリックで展開しないUIは改善キューへ |

## 4. §8.2 主要フローチェックリスト (12項)

| # | 項目 | 結果 | 証拠/メモ |
|---|------|------|-----------|
| 1 | 初期設定: フォルダ登録→ライブラリ表示 | **合格** | 2026-10-13 エージェント検証 (W-A: 登録走査9件即時) |
| 2 | 登録 (走査): 追加・リネーム・削除反映・フォルダ監視 | **合格** | 2026-10-13 エージェント検証 (W-A: 追加2秒/削除2.2秒/リネーム追従)。**不具合1**=検索→ライブラリ復帰後のみ表示停止 → WP-W-BUGS Fix2修正・**Windows実機再検証済み** (追加2秒/削除2秒反映・検索復元契約も保全: set_catalog_source("search")で37件復帰) |
| 7 | エクスポート: 選択画像コピー→内容一致 | **合格** | 2026-10-13 エージェント検証 (W-B: D:→C:越え・SHA256一致。§8.3-2同時合格) |
| 8 | バックアップ: DBバックアップ作成 | **合格 (修正後)** | W-C: 素main 3/3 FAIL (os error 5 — 読み取り専用ハンドルfsync・Windows固有) → WP-W-BUGS Fix1 (OpenOptions read+write) で成功: **Windows実機再検証済み** (フルDB 860,160B・2世代 + .gento 1アセット/4ファイル・新規単体テスト write_file_durable_syncs_with_write_access Windows PASS) |
| 9 | 復元: バックアップから復元 | **合格** | W-D (.gento経由・修正ビルド): ★5→バックアップ→★1→復元→applied=4・InfoPanel即時★5。グリッド評価バッジはライブラリ再スキャンで収束 (キュー#14) |
| 11 | 外部アプリ: 「フォルダーで表示」(Explorer選択)・「既定のアプリで開く」 | **合格** | W-E: 「OSのファイルマネージャーで表示」でExplorerが対象選択 (COM SelectedItems実証)、「外部アプリで開く」でフォト起動 |
| 12 | AI設定: API key→keyring (Credential Manager `gento` エントリ) | **合格** | W-F: ダミーkey保存→`…gento` (LegacyGeneric) 登録→再起動後読み出しOK→プロファイル削除で消滅。平文漏洩なし |
| 3 | 一覧: グリッド・スクロール・サムネイル・サイズ切替 | **合格** | 2026-10-13 W4b修正後「無事動作」報告 (サムネイル表示)。ドライブ列挙・ラベルクリック展開 (W4a) も同時確認 |
| 4 | ビューア: Enter/Space/ズーム | **合格** | 2026-10-13 W4b修正後「無事動作」報告 (画像表示復旧) |
| 5 | 比較: 2枚/4枚 | **合格** | 2026-10-13 ユーザー実機確認 |
| 6 | 評価: ★・選別・お気に入り・タグ | **合格** | 2026-10-13 ユーザー実機確認 (フィルタ即時適用 `cbd3776` も同時確認済み) |
| 10 | **ごみ箱**: ファイル→ごみ箱 + 元に戻す (**要重視** — §7.5 trashinfo無し環境のundo特定が未検証) | **合格 (既知制限として受理)** | 2026-10-13 W4d修正後: エラー解消・成功扱い+⚠案内・Recycle Binへ移動確認。「元に戻す」はWindowsでは不可 (エクスプローラーから手動復元) = WP-W4d設計どおり。上書きコピー (⚠退避案内+Ctrl+Z「元に戻しました⚠」) もユーザー確認済み |

## 5. §8.3 Windows特有の確認 (5項)

残項目 (§8.2-2/7/8/9/11/12 + §8.3全部) の実行手順書:
**`docs/dev/windows-agent-checklist.md`** (AIエージェント実行用 — テストデータ準備・
PowerShell検証・証跡フォーマット・クリーンアップ込み)。

| # | 項目 | 結果 | 証拠/メモ |
|---|------|------|-----------|
| 1 | 長いパス (MAX_PATH超 260文字) の登録・表示・エクスポート | **合格** | W-G: 273文字パスで表示・ビューア・構造保存エクスポート・SHA256一致 |
| 2 | ドライブ境界: 別ドライブへのエクスポート・移動 | **合格** | W-B (§8.2-7と同時): D:→C:エクスポート |
| 3 | Windows file ID無し (§2.3): size+mtimeトークン判定 (リネーム後評価追従) | **合格** | W-H: アプリ外リネーム後も★4+お気に入り維持 |
| 4 | UNICODEパス: 日本語・絵文字の登録・表示・リネーム・エクスポート | **合格** | W-I: アプリ内リネーム`画像_改名🐱.png` (U+1F431 codepoint検証)・エクスポート保全 |
| 5 | WebView2 Evergreen で上記全操作成立 | **合格** | W-J: Runtime 155.0.4283.45 でW-A〜W-I全部下 |

## 6. 発見済みの残課題キュー (WP-W4候補)

1. ~~[UX] フォルダツリーのラベルクリックでも展開~~ → **WP-W4a実装済み・Windows実機確認済み** (2026-10-13)
2. ~~Windowsでルート一覧をドライブ列挙に~~ → **WP-W4a実装済み・Windows実機確認済み** (2026-10-13)
3. **`devtools` featureが `tauri/devtools` を伝播していない** (releaseビルドで検証コンソールが開けない。`--features "devtools,tauri/devtools"` で回避可)
4. `registeredStateFor` のパス区切り想定が `/` 前提 (Windowsの `\` で登録バッジ・配下判定が不正の可能性)
5. Windowsでの `cargo test` がテストモジュール内unix専用コードでコンパイル不可 (protocol/fileops/fsview/library/export)
6. gento-platform のWindows警告3件 (cosmetic)
7. `gento-embedding-worker` の `.exe` 解決 + インストーラ同梱 (Linux共通の既知欠落。意味検索)
8. ~~正規repoのCargo.lockをnumkong 7.8.5へ更新するか判断~~ → **WP-NUMKONG-785実施 (2026-10-13, ユーザー承認)**: lock 3行のみのbump、Linuxゲート全緑 (workspace test / clippy -D warnings / tauri build 3バンドル、numkong 7.8.5をコンパイル確認)。licenses.md再生成 (licenses_notice testが強制)。Windows側は `git restore Cargo.lock` → `git pull` でlocal dirty解消 (numkong再ビルド不要)
9. 上流報告候補: numkong 7.8.5のcc-rs/MSVC静的リンク経路が `__imp_nk_*` で壊れる件 (DLL配布想定のdllimport)
10. ~~Windowsでカスタムプロトコル画像がすべて破損~~ → **WP-W4b実装済み・Windows実機確認済み** (2026-10-13。WebView2は `{http|https}://{scheme}.localhost` 形式 required。`convertFileSrc` のencodeURIComponentに注意 — protocol.ts `schemeBase` プローブ方式。CSPへorigin追加)
11. ~~Windowsでembeddingワーカーが起動せず進捗0~~ → **resolve_worker_path EXE_SUFFIX修正 (67e2249)・実機確認済み**。**インストール版も解決**: WP-W-WORKER (2026-10-13) — `scripts/prepare-worker.mjs` をbeforeBuild/DevCommandから実行し `bundle.externalBin` で同梱。Linux側 `pnpm tauri build` で deb `/usr/bin/gento-embedding-worker` + AppImage `usr/bin/` 同梱を実証済み。**Windows インストール版でも意味検索E2E確認済み (2026-10-13 ユーザー実機 → 完結)**
12. **[minor UX] セッション内で埋め込み→索引構築が完了しても検索モードセグメント (ハイブリッド/意味) が再有効化されない場合がある** (再起動で解消 = 索引ロード→ready経路は正常。Linuxでも発生しうるフロント状態同期バグ。UIUX-7の既知競合修正の別経路残り)
13. **[既知ドリフト] 現行Cargo.lockはcargo 1.96.0 resolverの固定点ではない** (WP-NUMKONG-785発見 — 任意の`cargo update -p X`でtempfile→getrandom 0.3.4フリップ+windows-sys系12エッジの正規化が混入。package追加なし・ビルド動作不変)。次回lock触るWPで正規化を受け入れるか固定化するか方針決定が必要
14. **[UX] .gento/DB復元完了後にカタログスナップショットが更新されず、グリッドの評価バッジが旧値のまま** (InfoPanel/DBは即時正。ライブラリ再スキャンで収束 — エージェント検証W-D観察)。復元完了時にスナップショット反映をトリガーすべき
15. **[UX] エクスポートのdry-run: 出力先dirが実在しないとプレビューが完了せずエラーも出ない** (既存dirなら即時 — W-B/W-G観察)。dir自動生成か明示エラーが必要
16. **[docs] 同一内容ファイルは1アセットに統合され、セルのpath (reveal/InfoPanel) は「代表ファイル」になる** — 代表ファイル選択規則のドキュメント化が必要 (W-E観察: 検証の同一PNGで全ファイル同一アセット化)
17. **[minor] 内容を書き換えたファイルのサムネイルが旧キャッシュのままfail扱い (3件)、再起動後に再生成で解消** — 競合検出の改善候補。記録のみ
18. ~~検索ボタンが検索完了後も「検索中…」無効のまま固まる~~ → **WP-UX-SEARCHRUN修正・Windows実機確認済み** (検索完了→ボタン復帰): SearchStore (通常クラス) の`running`/`lastResult`をテンプレートpropsが直接参照すると初回しか評価されないため、`searchEvidenceTick`橋渡しgetter (`searchStoreRunning()`等) 経由に変更 (8箇所)

## 6.1 エージェント検証で発見・修正した不具合 (2026-10-13 WP-W-BUGS)

- **不具合1 (→Fix2)**: 検索→「ライブラリを表示」復帰後、監視更新がUIへ反映されない (`switch_catalog_source`が`search`表示状態を落とさず`apply_library_snapshot`のemit条件が恒久false)。**修正**: `search_display_active`フラグ分離 (searchデータは`set_catalog_source("search")`復元契約のため保持) + `should_publish_library_snapshot`純粋関数化 + 回帰テスト2件。Codexレビュー2巡 (reject→approve)
- **不具合2 (→Fix1)**: 全DB/.gentoバックアップがWindowsで必ずos error 5 (読み取り専用ハンドルの`sync_all` — `FlushFileBuffers`はGENERIC_WRITE必須・Linuxでは再現しない)。**修正**: `verify_and_publish`/`write_file_durable`両箇所を`OpenOptions read+write`へ + 契約テスト
- エージェント検証報告・証跡一式: **repo外ローカル保管** (`docs/dev/gento-win-check/` はgitignore — cmdkeyダンプ・実パス等のPIIを含むため非コミット。要約は本節と§4の表に反映済み)

## 7. 判定 (実施完了時に記入)

- **完了状態: `done-with-notes`** (2026-10-13 確定)
  - §8.2 12項目全て合格 (10: ユーザー実機・エージェント検証。8: WP-W-BUGS修正後に実機合格)。§8.3 5項目全て合格 (エージェント検証)。
  - Windowsビルド手順確立 (`docs/dev/windows-build.md`)・worker自動同梱 (`453ea74`)・numkong 7.8.5正規化 (`8216faa`)・実bug 3件修正 (W4dごみ箱・W-BUGS 2件・SEARCHRUN — 全てWindows実機確認済み)。
  - notes (完了を妨げない既知事項): 残課題キュー #3〜#6, #12〜#17 (小規模UX・ドキュメント・保守性。#12のみ軽微な状態同期で再起動で解消)。ごみ箱のアプリ内undoはWindows非対応 (設計どおり・エクスプローラー手動復元で代替)。
  - 検証方式: ユーザー実機 (§8.2-3,4,5,6,10) + AIエージェント実機 (残項目 — `docs/dev/windows-agent-checklist.md`手順、証跡はrepo外PII対策済み) + Linux側PM再検証。
