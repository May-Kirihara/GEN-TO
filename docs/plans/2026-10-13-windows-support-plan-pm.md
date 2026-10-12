# Windows実機対応 PM計画 (WIN-1)

- 作成日: 2026-10-13 (rev.2 — Codex計画レビュー1回目 (3 blocker) を反映)
- PMエージェント作成。ユーザーがWindows実機 (10/11 x64) を用意済みで、
  最初のWindowsビルド確立と platform-notes §8 チェックリスト実施を目指す。
- リスク分類: **high-risk** — ビルド環境変更 (dav1dネイティブ依存の提供方式) を含み、
  状況によっては manifest・ビルド設定・コード修正に波及するため、計画レビューと
  証拠ベースの進行を全程適用する。
- 前提知識: `docs/dev/platform-notes.md` §1.2 (Windows cross-check不可能の記録)・
  §2 (Windows既知制限)・§8 (手動検証チェックリスト)。
- レビュー記録: `/tmp/codex-plan-review.md` (2026-10-13, approve with changes)。

## 1. 背景

- 正規開発・検証環境はLinuxのみ。Windows向けには (a) `gento-platform` による
  フォールバック実装、(b) `tauri.conf.json` の nsis/msi targets + icon.ico、
  (c) §8チェックリスト整備まで済んでいる (WP-17-1/2)。
- 未達は「Windows実機での最初のビルド」そのもの。最初の壁は `dav1d-sys` 0.8.3
  (AVIFデコード。`image` crate の `avif-native` feature 経由) が pkg-config +
  システム `libdav1d >= 1.3.0` を要求すること。Windowsには両方とも無い
  (ユーザー実測のpanic: `build.rs:82` `PkgConfig(...)`)。
- 2番目以降の潜在的壁 (区別して管理する — コンパイル/バンドル/インストール/
  実行時の4層): コンパイル層は `libsqlite3-sys` (bundled C・VS要件)・`usearch`
  (C++17)・`webp`/`blake3`。バンドル層は NSIS/WiX (MSIはVBSCRIPT featureが
  別要件)・`gento-embedding-worker` の同梱欠落。実行時層は `ort` ONNXバイナリ
  取得 (ビルド時DL)・WebView2・ゴミ箱undo (trashinfo走査はXDG固有)。

## 2. 目標・スコープ

**目標**: ドキュメント化された手順で Windows 11 x64 上の
`pnpm --dir apps/desktop tauri build` が成功し、NSIS/MSI が生成されること。
インストーラからインストールしたアプリが起動し、§8 チェックリスト (12+5項目)
を実施して結果を記録、発見事項を修正または既知制限として明文化すること。

**非目標 (本フェーズでは扱わない)**:
- macOS対応・コード署名・自動更新
- Windows向けE2E自動化 (現行E2EはXephyr/X11依存。手動チェックリストで代替)
- CI (GitHub Actions等) の導入
- 依存のバージョン変更・追加。**例外**: Windowsビルドが特定依存で閉塞した
  場合は、PMが調査の上、別承認WP (計画改訂) として処理する。本計画のスコープ
  内では実施しない。

## 3. 設計: dav1d提供方式

### 採用: Route B — dav1d-sysのソースビルド機構をenv varで有効化

`dav1d-sys` 0.8.3 の build.rs は `system_deps::Config::add_build_internal("dav1d", ...)`
で「dav1d 1.5.0 をgit clone → meson+ninjaで静的ビルド (`-Ddefault_library=static`) →
内部pkg-configで取り込み」というクロージャを登録済み。既定では発火しないが、
system-deps 7.0.8 は環境変数で発火を制御する (cargoキャッシュのソースで確認済み):

```
SYSTEM_DEPS_DAV1D_BUILD_INTERNAL=auto
```

- `auto`: pkg-configで見つかればシステム版、見つからなければソースビルドへ
  フォールバック。**この変数はWindowsビルドセッション内でのみ設定する**
  (Linuxビルドでは設定しない — Linux側の挙動は完全に不変。仮にLinuxで設定
  した場合、システムdav1d不在機ではエラーからソースビルドに挙動が変わる)。
- **必要ツールーチェーン** (WP-W1ガイドに手順化):
  - VS Build Tools 2022 — workload 「C++によるデスクトップ開発」(cl.exe・
    lib.exe。他のC依存でも必須)
  - `git` (dav1d 1.5.0 のclone。code.videolan.orgへ到達できること)
  - `meson` + `ninja` (例: `pip install meson ninja`)
  - `nasm` **≥ 2.14** (dav1dのx86 asm要件。PATHへの追加を確認)
  - **`pkg-config` 実行ファイル** (例: `choco install pkgconfiglite` または
    MSYS2 `pkgconf`) — ソースビルド後の取り込み
    (`from_internal_pkg_config`) が内部でpkg-configコマンドを起動するため、
    Route Bでも必須 (Codexレビュー blocker #1)
  - ビルドは **x64 Native Tools Command Prompt** から実行 (mesonがcl.exeを
    発見できるよう。rustc自身はvswhereで自律発見するがmesonはPATH由来)
- 静的リンクとなる見込みだが、**「dav1d.dll不要」は観測結果で確認する**
  (生成exeの依存検査: `dumpbin /dependents gento-tauri.exe` に dav1d.dll が
  含まれないこと)。これは受け入れ基準4に含める。
- **エラー読みの注意**: dav1d-sysの `runner!` マクロはサブプロセスの終了
  コードを検査しない (spawn失敗のみpanic)。meson/ninjaのエラーは最初の
  エラー行が正体で、最後のpanicは結果。ユーザー報告は最初のエラーを含む
  全文logとする。

### 代替案 (Route Bが失敗した場合のフォールバック): Route A — vcpkg

`vcpkg install dav1d:x64-windows-static-md` + pkg-config + `PKG_CONFIG_PATH`
指向。**成立証明手順**を含める: `PKG_CONFIG_PATH` 設定状态下で
`pkg-config --modversion dav1d` が `1.3.0` 以上を返すこと、および
`pkg-config --libs dav1d` が静的libを指すことを確認してからcargoを実行する
(vcpkg installの成功だけでは本ビルドが利用可能とは言えない — Codex指摘)。
vcpkgがnasm等を自己解決する利点がある一方、ブートストラップとtriplet運用の
学習コストがかかる。Route B最初のビルドがmeson/MSVC固有の問題で失敗した
場合に切り替える。

### 却下した案

- **`avif-native` featureの削除**: 全プラットフォームでAVIFデコードを失う
  プロダクト機能低下。platform-notes §7.2 にAVIF probe仕様が存在し、
  画像ファイラーの対応形式として確立済み。
- **ビルド済みバイナリのリポジトリ内ベンダリング**: バイナリblobsの管理・
  更新ポリシーが未整備で、リポジトリ衛生上の新規リスク。
- **`.cargo/config.toml [env]` への恒久化 (当面)**: `[env]` はtarget条件分岐が
  不能で、Linux寄与者のビルド挙動も変える。実測でRoute Bが確立してから
  別WP (WP-W5) として恒久化可否を判断する。

## 4. 受け入れ基準

各項目に観測証拠を伴うこと。チェックリスト項目の結果は
**実施 / 合格 / 不合格 / 既知制限として受理** の4値で記録する
(「記録した」だけで中核フロー不合格のまま完了扱いにしない)。

1. `docs/dev/windows-build.md` の手順でクリーンな Windows 11 x64環境の
   `pnpm --dir apps/desktop tauri build` が成功する。
2. NSIS (`.exe`) と MSI (`.msi`) 両方が生成される (MSIはVBSCRIPT要件を
   満たした上で。NSIS単独でもビルド継続可能なコマンドをガイドに併記)。
3. **インストーラ (NSIS/MSIどちらか) からインストールしたアプリが起動し、
   グリッドが描画される**ことの証拠 (スクリーンショット)。
   `target/release` の素exe確認だけでは足りない。
4. 生成物の依存検査で dav1d.dll が不要 (静的リンク) であることを確認。
5. §8チェックリスト全項目の結果 (4値+証拠) が
   `docs/handoff/2026-10-13-windows-verification.md` に記録される。
   環境証拠 (Windowsバージョン・各ツールのバージョン・ビルドコマンドと
   実行環境・成果物パス・インストール手段) を含む。
6. 発見した欠陥は修正されるか、platform-notes §2 の既知制限として明示的に
   受理される。**既知の初期キュー**: `gento-embedding-worker` の同梱欠落
   (§6 WP-W4参照)。

## 5. テスト戦略

- **ビルドゲート**: Windows実機での `cargo check` → `tauri build` →
  インストーラインストール→起動。
- Linux側の回帰確認は本フェーズ中のコード変更時に `cargo test --workspace` +
  `cargo clippy --workspace --all-targets -- -D warnings` を実施 (既定ゲート)。
- **受け入れ**: §8チェックリスト (手動。Windows E2E自動化は非目標)。
- **イテレーション上限**: 同一失敗クラスタへの修復は3回まで。超えたら
  証拠を保全し partial/blocked を報告 (第二失敗則の適用)。

## 6. 実装計画 (ワークパッケージ)

- **WP-W1 (PM): Windowsビルドガイド作成** — `docs/dev/windows-build.md`。
  Route B主案 (§3のツールチェーン全部+個別動作確認コマンド)・Route A
  フォールバック (成立証明手順付き)・MSI/VBSCRIPT要件・既知の落とし穴
  (最初のエラー読み等)。docsはPM所有のためPMが直接作成。
- **WP-W2 (ユーザー実行): 最初のビルド試行と環境証拠のhandoff** — ガイドに
  沿ってWindows実機でビルド。報告内容: 環境証拠 (Windows/各ツールの
  バージョン)・コマンド・stderr全文 (最初のエラーを含む)。PMが分類し、
  docs修正はPMが、コード修正はimplementer WPとして委譲。
- **WP-W3 (ユーザー実行 + PM記録): インストール済みアプリのチェックリスト実施** —
  §8の結果 (4値+証拠) をhandoff文書に記録。失敗項目はWP-W4へ。
- **WP-W4 (implementer, 発見駆動): 欠陥修正** — 発見された欠陥ごとにスコープを
  切って委譲。**初期キュー (ビルド前に判明済み)**:
  1. `resolve_worker_path()` がWindowsで `.exe` サフィックスを考慮しない
     (embedding.rs L1107-1133。target/release配下でも作動しない)
  2. `gento-embedding-worker` が `tauri.conf.json` に `externalBin` 登録が無く
     **Linux/Windows問わずインストーラ版に含まれない** (意味検索がインストール
     済みアプリで動かない既知欠落 — Codex blocker #3。修正は本フェーズでも
     実施するが、範囲が広がる場合は既知制限として記録し別計画へ分割する)
  アクセプタンスは「該当項目がチェックリスト上合格」+ Linux側回帰ゲート。
- **WP-W5 (implementer, 証拠後): ビルド設定の恒久化判断** — Route B確立後に
  `.cargo/config.toml [env]` 等への恒久化を実施するか否かを判断し、実施する
  場合は独立したレビュー済み変更とする。

## 7. 互換性・ロールバック

- フェーズ1 (WP-W1/W2) はリポジトリのコード・manifestに触れない (docsのみ)。
  ロールバック = docs差し戻しで完結。
- WP-W4/W5でリポジトリを変更する場合、各WPでスコープ・受入・ゲートを定義し、
  Linux側 `cargo test`/`clippy` を必須ゲートとする。

## 8. リスクと假定台帳

| # | 仮定 | 根拠 | 反証条件 | 影響 |
|---|------|------|----------|------|
| A1 | meson+ninja+nasm(≥2.14)+git+pkg-configでdav1d 1.5.0がMSVCビルドできる | dav1d-sysが当該クロージャを同梱、dav1d上流はMSVC対応 | 初回ビルドがmeson/MSVC固有エラーで失敗 | Route Aへ切替 (+数イテレーション) |
| A2 | repoのRustコードにWindows固有のコンパイルエラーは少ない | **弱い証拠**: core/platform/searchのcross-check緑はdarwin向け実績のみ。db/ai/image/embedding/tauriのWindows checkは一度も走っていない (platform-notes §1) | 上位crateのcheck失敗 | implementer修正WP (小規模想定だが未知) |
| A3 | §8チェックリストは概ねパスする — **ただしゴミ箱undo (trashinfo走査はXDG固有・Windows Recycle Binに trashinfo は無い) と keyring/ACL は失敗濃度が高いと見る** | フォールバック実装済み・純関数テスト緑 / ゴミ箱系はgento-platform L246以降がXDG前提 | 中核フロー (走査・表示・ゴミ箱・undo) の実行時失敗 | 修正WP、または既知制限として§2へ格下げ判断 |
| A4 | `ort` (ONNX事前ビルドバイナリ取得)・`usearch` (C++)のMSVCビルドは通る | いずれもWindows公式サポートあり | 該当crateのビルド失敗 | PMが調査し**別承認WP**として処理 (本計画スコープ外。§2例外規定) |
| A5 | `pkgconfiglite`/MSYS2 pkgconfで `.pc` 読み取りが成立する | pkg-config互換の標準的役割 | 内部pkg-config取り込みステップの失敗 | pkgconf入手手段の切替 (MSYS2/choco) |
| A6 | NSIS/MSIツールチェーンはTauriが自動ダウンロードする (MSIはVBSCRIPT featureが別要件) | Tauri v2標準挙動・公式前提文書 | light.exe失敗等 | ガイドの診読分岐 (NSIS単独ビルド `--bundles nsis` で継続) |
| A7 | ビルド時ネットワーク取得 (crates.io・code.videolan.org・NSIS/WiX DL・ort ONNX binaries) が可能 | 開発環境では常時成立 | プロキシ/ファイアウォール阻害 | 手動DL手順のガイド追記 |
| A8 | dav1d静的リンクが成立しdav1d.dll同梱が不要になる | `-Ddefault_library=static` 指定 | `dumpbin /dependents` で dav1d.dll が出る | resources/externalBinでのDLL同梱検討 (別WP) |
| A9 | embedding worker同梱は本フェーズで最小修正可能 | `.exe` サフィックス対応+externalBin登録は局所的 | バンドル挙動の調査で範囲拡大 | 既知制限として記録し、別計画へ分割 |

## 9. ドキュメント/handoff影響

- 新規: `docs/dev/windows-build.md` (WP-W1)・
  `docs/handoff/2026-10-13-windows-verification.md` (WP-W3)。
- 更新 (最終): README (Windowsセットアップ節)・platform-notes (検証状況の改訂)。
- GaOTTT writeback: 確立したWindowsビルド手順の本質 (失敗教訓含む) を最終時に記録。

## 10. 完了定義

§4の受け入れ基準1〜6を満たし、final reportで `done` または
`done-with-notes` (残存既知制限あり) を報告すること。
