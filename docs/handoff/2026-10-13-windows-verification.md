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
| 1 | 初期設定: フォルダ登録→ライブラリ表示 | 未実施 | |
| 2 | 登録 (走査): 追加・リネーム・削除反映・フォルダ監視 | 未実施 | |
| 3 | 一覧: グリッド・スクロール・サムネイル・サイズ切替 | **合格** | 2026-10-13 W4b修正後「無事動作」報告 (サムネイル表示)。ドライブ列挙・ラベルクリック展開 (W4a) も同時確認 |
| 4 | ビューア: Enter/Space/ズーム | **合格** | 2026-10-13 W4b修正後「無事動作」報告 (画像表示復旧) |
| 5 | 比較: 2枚/4枚 | 未実施 | |
| 6 | 評価: ★・選別・お気に入り・タグ | 未実施 | |
| 7 | エクスポート: 選択画像コピー→内容一致 | 未実施 | |
| 8 | バックアップ: DBバックアップ作成 | 未実施 | |
| 9 | 復元: バックアップから復元 | 未実施 | |
| 10 | **ごみ箱**: ファイル→ごみ箱 + 元に戻す (**要重視** — §7.5 trashinfo無し環境のundo特定が未検証) | 未実施 | |
| 11 | 外部アプリ: 「フォルダーで表示」(Explorer選択)・「既定のアプリで開く」 | 未実施 | |
| 12 | AI設定: API key→keyring (Credential Manager `gento` エントリ) | 未実施 | |

## 5. §8.3 Windows特有の確認 (5項)

| # | 項目 | 結果 | 証拠/メモ |
|---|------|------|-----------|
| 1 | 長いパス (MAX_PATH超 260文字) の登録・表示・エクスポート | 未実施 | |
| 2 | ドライブ境界: 別ドライブへのエクスポート・移動 | 未実施 | |
| 3 | Windows file ID無し (§2.3): size+mtimeトークン判定 (リネーム後評価追従) | 未実施 | |
| 4 | UNICODEパス: 日本語・絵文字の登録・表示・リネーム・エクスポート | 未実施 | |
| 5 | WebView2 Evergreen で上記全操作成立 | 未実施 | |

## 6. 発見済みの残課題キュー (WP-W4候補)

1. ~~[UX] フォルダツリーのラベルクリックでも展開~~ → **WP-W4a実装済み・Windows実機確認済み** (2026-10-13)
2. ~~Windowsでルート一覧をドライブ列挙に~~ → **WP-W4a実装済み・Windows実機確認済み** (2026-10-13)
3. **`devtools` featureが `tauri/devtools` を伝播していない** (releaseビルドで検証コンソールが開けない。`--features "devtools,tauri/devtools"` で回避可)
4. `registeredStateFor` のパス区切り想定が `/` 前提 (Windowsの `\` で登録バッジ・配下判定が不正の可能性)
5. Windowsでの `cargo test` がテストモジュール内unix専用コードでコンパイル不可 (protocol/fileops/fsview/library/export)
6. gento-platform のWindows警告3件 (cosmetic)
7. `gento-embedding-worker` の `.exe` 解決 + インストーラ同梱 (Linux共通の既知欠落。意味検索)
8. 正規repoのCargo.lockをnumkong 7.8.5へ更新するか判断 (Windows実績と一致させるため。要implementer WP + Linuxゲート)
9. 上流報告候補: numkong 7.8.5のcc-rs/MSVC静的リンク経路が `__imp_nk_*` で壊れる件 (DLL配布想定のdllimport)
10. ~~Windowsでカスタムプロトコル画像がすべて破損~~ → **WP-W4b実装済み・Windows実機確認済み** (2026-10-13。WebView2は `{http|https}://{scheme}.localhost` 形式 required。`convertFileSrc` のencodeURIComponentに注意 — protocol.ts `schemeBase` プローブ方式。CSPへorigin追加)
11. ~~Windowsでembeddingワーカーが起動せず進捗0~~ → **resolve_worker_path EXE_SUFFIX修正 (67e2249)・実機確認済み** (target\release直起動で意味検索が完全動作: 埋め込み37件・索引構築・モード選択)。**インストール版では未解決** — ワーカー同梱 (externalBin + ビルド統合) は本項の残作業
12. **[minor UX] セッション内で埋め込み→索引構築が完了しても検索モードセグメント (ハイブリッド/意味) が再有効化されない場合がある** (再起動で解消 = 索引ロード→ready経路は正常。Linuxでも発生しうるフロント状態同期バグ。UIUX-7の既知競合修正の別経路残り)

## 7. 判定 (実施完了時に記入)

- 完了状態: (未定 — チェックリスト完了後に done / done-with-notes / partial を判定)
