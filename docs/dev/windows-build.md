# Windowsビルド手順 (WIN-1 / Route B)

最終更新: 2026-10-13 (**Windows 11実機でビルド・インストール・起動まで確認済み** — 検証記録: [docs/handoff/2026-10-13-windows-verification.md](../handoff/2026-10-13-windows-verification.md))

> **2026-10-13実績環境**: Windows 11 + Visual Studio 18 Community (MSVC 14.51) +
> NASM 3.02 + meson 1.12.1 + numkong 7.8.5 (ローカルlock) + dav1d 1.5.0 (静的ビルド)。
> 日本語Windows (CP932) では§2の `CFLAGS`/`CXXFLAGS` が必須。

対象: Windows 10/11 x64。このリポジトリの正規検証環境はLinuxであり、
Windowsビルドは本手順で初めて確立する (実績は
[docs/handoff/2026-10-13-windows-verification.md](../handoff/2026-10-13-windows-verification.md) に記録する)。

最初の壁は `dav1d-sys` 0.8.3 (AVIFデコード。`image` crateの `avif-native`
feature経由)。本手順は dav1d-sys が内蔵するソースビルド機構 (dav1d 1.5.0 を
meson+ninjaで静的ビルド) を環境変数で有効化する方式 (Route B)。

```
SYSTEM_DEPS_DAV1D_BUILD_INTERNAL=auto
```

`auto` = pkg-configでシステムdav1dが見つかればそれを使い、無ければソース
ビルドへフォールバック。Windowsでは常に後者が選ばれる。**この変数は
Windowsビルドのセッション内でのみ設定すること** (Linux側で設定する必要は
なく、設定すべきでない — 計画§3参照)。

## 1. 前提ツールーチェーン (一回のみ)

| ツール | 導入方法 | 確認コマンド |
|---|---|---|
| Rust (MSVC target) | https://rustup.rs (既定で `x86_64-pc-windows-msvc`) | `rustc -vV` |
| Node.js + pnpm | https://nodejs.org 等 + `corepack enable` または `npm i -g pnpm` | `node -v` / `pnpm -v` |
| VS Build Tools 2022 | Visual Studio Installer で **workload「C++によるデスクトップ開発」** を選択 | `cl` (Native Toolsプロンプト内) |
| **MSIビルド用 (任意)** | Visual Studio Installer の個別コンポーネントで **「Visual C++ VBScriptのツールセット」(VBSCRIPT)** を追加。無いとMSIの `light.exe` が失敗する | — |
| git | https://git-scm.com | `git --version` |
| meson / ninja | `pip install meson ninja` (要Python 3) | `meson --version` / `ninja --version` |
| nasm (**≥ 2.14**) | https://www.nasm.us/ (インストーラはPATH追加を有効化) | `nasm -v` |
| pkg-config | `choco install pkgconfiglite` (要choco) または MSYS2 `pacman -S mingw-w64-ucrt-x86_64-pkgconf` | `pkg-config --version` |

補足:

- **nasm・pkg-config は PATH に入っている必要がある**。winget経由のnasmは
  PATHに自動追加されないため注意。
- pkg-configはdav1dのソースビルド後の取り込み (`from_internal_pkg_config`)
  で内部起動されるため、**Route Bでも必須**。
- WebView2 Evergreen Runtime はWindows 11なら標準、Windows 10は大抵の場合
  既存 (アプリ実行に必要。ビルドには不要)。
- ビルド中にcrates.io・code.videolan.org (dav1d clone)・NSIS/WiXツール類・
  ONNX事前ビルドバイナリ (ort) のネットワーク取得が発生する。プロキシ環境
  では事前に到達確認すること。

## 2. ビルド手順

**必ず「x64 Native Tools Command Prompt for VS 2022」から実行する**
(mesonが `cl.exe` をPATH経由で発見する必要があるため。素のPowerShellでは
rustcはMSVCを自律発見できるがmesonはできない)。

```bat
cd <リポジトリパス>\apps\desktop
pnpm install

:: dav1dソースビルドを有効化 (セッション内のみ。綴り注意: SYSTEM_DEPS_DAV1D_BUILD_INTERNAL)
set SYSTEM_DEPS_DAV1D_BUILD_INTERNAL=auto

:: 日本語Windowsでは必須 (原因は§6参照): C全般とC++の両方に /utf-8
:: CXXFLAGSの -DNK_DYNAMIC= は usearch↔numkong 静的リンク用 (§6参照)
set CFLAGS=/utf-8
set CXXFLAGS=/utf-8 -DNK_DYNAMIC=
```

まず軽量の全体check (dav1dビルドを含む build script はこの時点で走る):

```bat
cd <リポジトリパス>
cargo check -p gento-tauri
```

成功したらreleaseビルド+バンドル:

```bat
cd apps\desktop
pnpm tauri build
```

- 成果物: `target\release\bundle\nsis\*.exe` と `target\release\bundle\msi\*.msi`
- MSIのVBSCRIPT要件を満たしていない/失敗する場合でも、NSIS単独で継続できる:
  `pnpm tauri build --bundles nsis`
- 初回はC依存 (sqlite bundled・usearch C++・dav1d等) のコンパイルで
  時間がかかる。

## 3. 静的リンクの確認 (dav1d.dll不要の実証)

Native Toolsプロンプトで:

```bat
dumpbin /dependents target\release\gento-tauri.exe | findstr /i dav1d
```

出力が空なら dav1d は静的リンク済み (DLL同梱不要)。`dav1d.dll` が出た場合は
計画A8の反証 — 報告すること。

## 4. エラー報告の約束

- **最初のエラー行が正体**。`dav1d-sys` の `runner!` マクロはサブプロセスの
  終了コードを検査しないため、git/meson/ninjaの失敗は連鎖的に後段へ流れ、
  最後のpanic (例: `build.rs:82`) は結果であって原因ではない。
- 報告に含めるもの:
  1. Windowsバージョン (`winver`) と各ツールのバージョン (§1の確認コマンド群)
  2. 実行コマンドと実行環境 (Native Toolsプロンプトか)
  3. **stderr全文** (最初のエラー行から。切り詰めない)
  4. 該当するなら `target\release\build\dav1d-sys-*\output` の内容

## 5. Route A フォールバック (Route Bがmeson/MSVC固有の問題で失敗した場合)

vcpkgでdav1dを導入し、pkg-config経由で発見させる:

```bat
git clone https://github.com/microsoft/vcpkg C:\vcpkg
C:\vcpkg\bootstrap-vcpkg.bat
C:\vcpkg\vcpkg install dav1d:x64-windows-static-md

:: pkg-configに見えることを先に証明する (成立確認してからcargo)
set PKG_CONFIG_PATH=C:\vcpkg\installed\x64-windows-static-md\lib\pkgconfig
pkg-config --modversion dav1d    :: 1.3.0 以上を返すこと
pkg-config --libs dav1d          :: 静的libを指すことを確認
```

この状態では `SYSTEM_DEPS_DAV1D_BUILD_INTERNAL` を**設定しない**
(pkg-config発見を優先させる)。

## 6. 既知の落とし穴

| 症状 | 原因と対処 |
|---|---|
| `The pkg-config command could not be found` | pkg-config未導入またはPATH不一致 (§1)。Route Bでも必要 |
| `Program 'nasm not found` / asm関連のmesonエラー | nasm未導入 or PATH未追加、またはバージョン < 2.14。**インストール後にプロンプトを開き直すこと** (PATH反映) |
| mesonが `cl` を見つけられない | 素のPowerShell/cmdで実行している。x64 Native Toolsプロンプトを使う |
| **numkong/usearchのC/C++ソースが全ファイルexit 2で失敗** (cargoログに実エラーが出ない) | **日本語WindowsのclがソースをCP932として解釈し、numkongヘッダのUTF-8コメントで構文解析が壊れる**。`set CFLAGS=/utf-8` (C: numkong) と `set CXXFLAGS=/utf-8` (C++: usearch) で解決。clのエラーはstdoutへ出るがcc-rsが破棄するため、診断は手動cl再現 (`cl ... -c c\dispatch_other.c > %TEMP%\err.txt 2>&1`) で行う |
| **リンクエラー `LNK2019: 未解決の外部シンボル __imp_nk_*`** | numkong 7.8.5が消費者に `dllimport` を与える設計 (DLL配布想定) なのに対し、我々は静的リンクしているため。`set CXXFLAGS=/utf-8 -DNK_DYNAMIC=` で `types.h` のdllimport分岐を短路して解消 |
| `light.exe` 失敗 (MSI) | VBSCRIPTコンポーネント未導入 (§1)。`--bundles nsis` で継続可 |
| code.videolan.orgへのclone失敗 | ネットワーク/プロキシ。A7 — 手動での事前cloneは不可 (OUT_DIR管理のため) ので環境側を解決する |
| ビルドは成功するが意味検索が動かない | 既知の初期キュー (計画§6 WP-W4): workerの `.exe` 解決とバンドル欠落。ビルド検証のブロッカーではない |
| cmdでディレクトリ移動が効かない | ドライブをまたぐ `cd` には `cd /d` が必要 |

## 7. Linux側への影響

なし。本手順の環境変数・ツールはWindowsビルドセッション内のみで使用し、
リポジトリのmanifest・lockfileには触れない (恒久化はWP-W5で別判断)。
