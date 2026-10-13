# Windows実機検証 — AIエージェント用チェックリスト (WIN-1 §8.2/§8.3 残項目)

- 作成: 2026-10-13 (PM) — `docs/dev/platform-notes.md` §8.2/§8.3 の残項目を
  AIエージェント (PowerShell + UI操作) が自律実行できる形に展開した実行手順書。
- 実行主体の想定: Windows 11 x64 上のエージェント (Claude Code等)。リポジトリは
  `D:\Devs\GEN-TO` (main最新)。PowerShell 5.1前提 (pwsh 7でも可)。
- 事実のみ報告すること。推測でPASSを出さない。不明時はBLOCKED+理由。
- 既に§8.2-3/4/5/6/10 (ごみ箱はW4d修正後) は人手検証済み。本書は
  **§8.2-2, 7, 8, 9, 11, 12 と §8.3-1〜5** が対象。

## 0. 全体ルール

1. **証跡ディレクトリ**: すべての証跡 (スクリーンショット・コマンド出力) は
   `%TEMP%\gento-win-check\` へ項目IDごとに保存する:
   ```powershell
   New-Item -ItemType Directory -Force "$env:TEMP\gento-win-check" | Out-Null
   ```
2. **スクリーンショット取得** (必要に応じて):
   ```powershell
   Add-Type -AssemblyName System.Windows.Forms,System.Drawing
   $b = [System.Windows.Forms.SystemInformation]::VirtualScreen
   $bmp = New-Object System.Drawing.Bitmap($b.Width, $b.Height)
   $g = [System.Drawing.Graphics]::FromImage($bmp)
   $g.CopyFromScreen($b.X, $b.Y, 0, 0, $bmp.Size)
   $bmp.Save("$env:TEMP\gento-win-check\<項目ID>-<連番>.png")
   ```
3. **UIラベルが不明なときはソースをgrepする** (UIは日本語・ラベルはソース正本):
   ```powershell
   Select-String -Path apps\desktop\src\App.svelte -Pattern "バックアップ" -Encoding UTF8
   ```
   主要UI: `App.svelte` (メニュー・ファイル操作)、`lib/shell/LibraryPanel.svelte`
   (登録)、`lib/shell/AiPanel.svelte` (AI設定)、`lib/shell/InfoPanel.svelte` (評価)。
4. **テストで本物のAPI keyは絶対に使わない** (§12はダミー文字列で登録検証のみ)。
5. **証跡の個人情報対策 (必須 — repoはpublic)**:
   - `cmdkey /list` は**全文ダンプしない** — `cmdkey /list | Select-String "gento"` 等
     対象エントリ周辺のみ抽出 (ダンプには他サービスの資格情報ターゲット・
     メールアドレスが並ぶ)。
   - 証跡テキストのユーザー名・実フォルダ名はマスク (`C:\Users\<user>` 等に置換)。
   - スクリーンショットは公開前に内容確認 (デスクトップの実フォルダ名・
     他ウィンドウが写っていないか)。
   - 証跡一式は `%TEMP%\gento-win-check\` または repoのgitignore対象
     (`docs/dev/gento-win-check/`) に置く — **コミットしない**。報告は
     report.mdの要約 (マスク済み) のみrepoに入れる。
5. アプリのDB・設定は `%APPDATA%\gento-spike\` (Roaming)。バックアップ既定Dirは
   DB近傍の `db-backups\`。開始前に念のため既存世代を記録しておく:
   ```powershell
   Get-ChildItem "$env:APPDATA\gento-spike" -Recurse -File |
     Select-Object FullName, Length, LastWriteTime |
     Format-Table -AutoSize | Out-File "$env:TEMP\gento-win-check\00-pre-state.txt"
   ```

## 1. ビルドと起動

```bat
cd /d D:\Devs\GEN-TO
git pull
set SYSTEM_DEPS_DAV1D_BUILD_INTERNAL=auto
set CFLAGS=/utf-8
set CXXFLAGS=/utf-8 -DNK_DYNAMIC=
set PATH=C:\Users\slick\AppData\Local\bin\NASM;%PATH%
cd apps\desktop
pnpm tauri build
```

- 成功物: `target\release\gento-tauri.exe` (+ NSIS/MSI)。**UI検証はexe直起動でよい**
  (高速化する場合 `pnpm tauri build --no-bundle` — バンドル検証済みなので可)。
- **コンソールから起動してstdoutログを取る** (成果物はrepo rootの`target\release\`:
  workspace直targetのため — `apps\desktop\src-tauri\target` ではない):
  ```powershell
   D:\Devs\GEN-TO\target\release\gento-tauri.exe *>&1 |
     Tee-Object "$env:TEMP\gento-win-check\app-stdout.log"
  ```
  (別セッションで起動した場合はログ取得を諦め、UI観察中心でよい)
- 起動済みインスタンスがある場合は終了してから (重複起動の状態検証は本書の対象外)。
- 任意 (高度): devブリッジを使う場合は
  `pnpm tauri build --no-bundle -- --features devtools` でビルドし、
  環境変数 `GENTO_DEV_BRIDGE=1` (厳密に"1") を設定して起動すると、F1〜F12の
  コマンドモード (`apps/desktop/src/lib/devtest.ts` 参照) と機械可読readout
  (document.title・`devtest:` ログ行) が有効になる。本書の手順はdevブリッジなしで
  完結するように書いている (任意の補助手段)。

## 2. テストデータ準備 (アプリ外・PowerShell)

テスト用ルート `D:\GentoWinCheck` を作成し、合成PNG (1x1) を必要数書く:

```powershell
$root = "D:\GentoWinCheck\library"
New-Item -ItemType Directory -Force $root | Out-Null
$png = [Convert]::FromBase64String(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==")
1..6 | ForEach-Object {
  [IO.File]::WriteAllBytes("$root\photo-$_.png", $png)
}
# UNICODE (§8.3-4): 日本語 + 絵文字
[IO.File]::WriteAllBytes("$root\画像_日本語.png", $png)
[IO.File]::WriteAllBytes("$root\🐱絵文字ファイル.png", $png)
# 監視検証 (§8.2-2) で追加する分はアプリ登録後に作る
```

長いパス (§8.3-1・260文字超):

```powershell
$deep = $root + ("\深い\" * 18)   # 約260文字超になることを確認
New-Item -ItemType Directory -Force $deep | Out-Null
[IO.File]::WriteAllBytes("$deep\奥の画像.png", $png)
($deep + "\奥の画像.png").Length   # >= 260 を確認して記録
```

別ドライブ宛先 (§8.3-2 — D:とC:の境界):

```powershell
New-Item -ItemType Directory -Force "C:\Users\slick\GentoWinCheck-export" | Out-Null
```

## 3. チェック項目

### W-A §8.2-2 登録 (走査) と監視

1. アプリ左の **LibraryPanel** から「追加」→ フォルダ選択ダイアログで
   `D:\GentoWinCheck\library` を登録 → ライブラリ表示へ切替、グリッドに
   9件 (photo-1..6 + 日本語 + 絵文字 + 奥の画像 ※サブフォルダ走査含む) 級が出る。
2. 走査完了後、PowerShellで**追加** → 数秒待ち → グリッドに反映:
   ```powershell
   [IO.File]::WriteAllBytes("D:\GentoWinCheck\library\watch-added.png",
     [Convert]::FromBase64String("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="))
   ```
3. **リネーム** (`Rename-Item ... watch-added.png watch-renamed.png`) → 反映。
4. **削除** (`Remove-Item ... watch-renamed.png`) → 消える。
- 判定: 追加/リネーム/削除がいずれもUIへ自動反映 (手動再走査不要)。
  反映が30秒待っても無い場合はFAIL (監視が効いていない)。
- 証跡: 各ステップ後のスクショ + `Get-ChildItem` 出力。
- 注意: 反映確認はグリッドの件数表示・画像の有無で行う (サムネは非同期なので
  白表示でも件数が合えば走査・監視としてはPASS)。

### W-B §8.2-7 エクスポート (コピー) — §8.3-2 ドライブ境界と同時

1. グリッドで `photo-1.png` を選択 → メニュー **ファイル > SD学習用エクスポート…**
2. ダイアログの指示に従い宛先を `C:\Users\slick\GentoWinCheck-export` へ。
   (ダイアログの形式・手順は表示文言に従う。対象はコピーされる)
3. ハッシュ一致で内容検証:
   ```powershell
   Get-FileHash "D:\GentoWinCheck\library\photo-1.png","C:\Users\slick\GentoWinCheck-export\photo-1.png" -Algorithm SHA256
   ```
- 判定: 宛先にファイルが存在し、両ハッシュが一致。D:→C: の
  ドライブ越えで成立すれば §8.3-2 もPASS。

### W-C §8.2-8 バックアップ

1. メニュー **ツール > 全データベースをバックアップ**
2. 成功通知に `DBバックアップを作成しました: <path> (保持N世代)` と出る。
3. ファイル実在確認:
   ```powershell
   Get-ChildItem "$env:APPDATA\gento-spike" -Recurse -Filter "*.sqlite*" |
     Select-Object FullName, Length, LastWriteTime
   ```
   (通知のpathが別場所ならそちらを確認)
- 判定: 通知のパスに世代ファイルが実在。
- 追加 (ルート単位): **ツール > ルートのバックアップ…** も1世代作成できるか
  (`.gento`形式 — これは§8.2-9復元の入力にも使う。時間があれば実施)。

### W-D §8.2-9 復元

事前にW-Cでバックアップを作っておく。手順 (メタデータ復元の往復):

1. 適当な画像 (例: `photo-2.png`) をアプリ内で★5に評価 (InfoPanelの★)。
2. **ツール > 全データベースをバックアップ** (★5状態を保全)。
3. ★評価を★1へ変更 (または★0クリア)。
4. **ツール > バックアップから復元…** → ダイアログの指示に従い直近の
   バックアップを選択 → 復元実行。
5. 復元完了後、`photo-2.png` が★5に戻っていることを確認。
- 判定: 復元後に評価がバックアップ時点へ戻る。表示が古い場合は
  表示切替 (別フォルダ→戻る等) で再読込してよい (その旨を証跡に記録)。
- 補足: `.gento` (ルート単位) 復元もW-Cで作成していれば同様に検証可。
  どちらか片方でよい (実施した方を報告)。

### W-E §8.2-11 外部アプリ連携

1. グリッドで `photo-1.png` 右クリック → **「OSのファイルマネージャーで表示」**
   (コンテキストメニュー。実ラベルはソース正本で確認:
   `Select-String -Path apps\desktop\src\**\*.svelte -Pattern "ファイルマネージャー"`)
2. Explorerウィンドウが開き、当該ファイルが選択状態になる。
   PowerShellで開いたウィンドウの場所を確認:
   ```powershell
   (New-Object -ComObject Shell.Application).Windows() |
     ForEach-Object { $_.LocationURL } | Select-String "GentoWinCheck"
   ```
3. **「外部アプリで開く」** → 既定の画像ビューア (フォト等) が起動する。
   プロセス起動を確認後、開いたウィンドウを閉じる:
   ```powershell
   Start-Sleep 3; Get-Process | Where-Object { $_.MainWindowTitle -match "フォト|photo|画像" } | Select-Object ProcessName, MainWindowTitle
   ```
- 判定: 両操作で期待アプリが開く (Explorerは対象フォルダ+選択、ビューアは画像表示)。

### W-F §8.2-12 AI設定 (keyring)

1. メニュー **ツール > 設定・診断** (またはAIパネル) → API key入力欄へ
   **ダミー** `sk-wincheck-dummy` を入力して保存。
2. 資格情報マネージャーにエントリが登録されたか:
   ```powershell
   cmdkey /list | Select-String -Context 2,2 "gento"
   ```
   またはGUI: `control /name Microsoft.CredentialManager` → Windows資格情報/
   汎用資格情報に `gento` エントリ。
3. アプリ再起動 → API key欄が復元されるか (keyring読み出し)。
4. **検証後、ダミーkeyを削除** (アプリの設定から、または資格情報マネージャーで
   `gento` エントリを削除)。
- 判定: 登録→`cmdkey`でエントリ確認→再起動後も読める→削除まで一貫。
  (AI呼び出し自体はダミーなので失敗してよい — 保存機構のみが対象)

### W-G §8.3-1 長いパス (MAX_PATH超)

1. §2で作成した深いパス (`奥の画像.png`, full path >= 260文字) が
   W-Aの走査でグリッドに出ていることを確認。
2. サムネイル表示・選択・Enter (ビューアで開く) が成立するか。
3. エクスポート (W-B同様、別dirへ) でハッシュ一致するか。
- 判定: 表示・ビューア・エクスポートが260文字超パスで成立
  (字句フォールバック§2.1の実地確認)。

### W-H §8.3-3 file IDなしの同一性 (size+mtime)

Windowsでは `os_file_id_of` がNoneのため、size+mtimeで同一トークン判定される。

1. アプリ内で `photo-3.png` に★4 + お気に入りを付ける。
2. PowerShellでリネーム (アプリ外):
   ```powershell
   Rename-Item "D:\GentoWinCheck\library\photo-3.png" "photo-3-renamed.png"
   ```
3. 監視反映後 (またはフォルダ再選択)、`photo-3-renamed.png` に★4・お気に入りが
   **追従している**ことを確認。
- 判定: リネーム後も評価が失われない (size+mtime同一性が機能)。
- 補足: リネームによって評価がリセットされる場合はFAILとして詳細 (DB側 or
  UI側のどちらで失われたか) を記録。

### W-I §8.3-4 UNICODEパス

1. §2の日本語・絵文字ファイル名がW-Aで表示されていることを確認。
2. アプリ内で日本語ファイル名の画像をビューア表示。
3. アプリ内リネーム機能がある場合は「画像_日本語.png」→「画像_改名🐱.png」へ
   変更し、ファイルシステム側で実ファイル名が正しく変わるか:
   ```powershell
   Get-ChildItem "D:\GentoWinCheck\library" -Filter "*改名*" | Select-Object Name
   ```
4. エクスポートしてファイル名が保全されるか (ハッシュも)。
- 判定: 表示・ビューア・リネーム・エクスポート全てでUNICODE名が化けず成立。

### W-J §8.3-5 WebView2 (観点)

- 上記W-A〜W-IがすべてWebView2 Evergreen上で成立していれば本項はPASS。
- 記録としてRuntimeバージョンを採取:
  ```powershell
  reg query "HKLM\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" /v pv
  ```

## 4. 報告フォーマット

`%TEMP%\gento-win-check\report.md` に以下で報告 (FAILは観察事実をそのまま):

```md
# Windows実機検証報告 (エージェント実行) — 日付
| 項目 | 結果 (PASS/FAIL/BLOCKED) | 証跡 | 備考 |
|------|------|------|------|
| W-A §8.2-2 | | | |
| W-B §8.2-7+§8.3-2 | | | |
| W-C §8.2-8 | | | |
| W-D §8.2-9 | | | |
| W-E §8.2-11 | | | |
| W-F §8.2-12 | | | |
| W-G §8.3-1 | | | |
| W-H §8.3-3 | | | |
| W-I §8.3-4 | | | |
| W-J §8.3-5 | | | |
## 発見した不具合 (観察・再現手順・期待)
## その他観察
```

## 5. クリーンアップ

1. アプリからテストルート `D:\GentoWinCheck` を登録解除 (LibraryPanel)。
2. `Remove-Item -Recurse -Force D:\GentoWinCheck` (エントリ残件はごみ箱運用でも可)。
3. `C:\Users\slick\GentoWinCheck-export` を削除。
4. ダミーAPI keyを削除 (W-Fで未削除なら)。
5. `%TEMP%\gento-win-check` は報告後に残置してよい (証跡)。
