# プラットフォーム別既知制限と検証状況 (WP-17-1 / WP-17-2)

最終更新: 2026-09-04 (WP-17-2: §6-§10追記)

GEN-TO MVPの正規開発・検証環境はLinux (x86_64) のみ。本書は
WP-17-1/WP-17-2時点で判明しているプラットフォーム差・制限・検証
カバレッジの正本である。計画書§25 (WP-17) の受け入れ条件
「OS別の既知制限を文書化する」「platform差を`gento-platform`へ隔離する」
に対応する。

## 1. クロスコンパイル検証のカバレッジ (2026-09-04時点)

開発環境はLinuxホスト。`rustup` target `aarch64-apple-darwin`
`x86_64-pc-windows-msvc` は導入済みだが、**C/ObjC/C++をコンパイルする
build scriptを持つ依存はホストにクロスCツールチェーンが無いため
コンパイルできない** (osxcross / cargo-zigbuild 等が未導入)。

### 1.1 darwin (aarch64-apple-darwin) — `cargo check` 結果

| crate | 結果 | 備考 |
|-------|------|------|
| gento-core | ✅ 緑 | E0617 (variadic mode_t) 修正済み |
| gento-platform | ✅ 緑 | statvfs/mtime_ns_of のdarwinコンパイルを含む |
| gento-search | ✅ 緑 | 純Rust依存のみ |
| gento-db | ❌ 環境不可 | `libsqlite3-sys` (bundled C) + `blake3` NEON C |
| gento-ai | ❌ 環境不可 | `blake3` NEON C (cc) |
| gento-image | ❌ 環境不可 | `dav1d-sys` (AVIF C) + `blake3` NEON C |
| gento-embedding | ❌ 環境不可 | `aws-lc-sys` (C) + `link-cplusplus`/`usearch` (C++) |
| gento-tauri | ❌ 環境不可 | `objc2-exception-helper` (Objective-C `try_catch.m` をccでコンパイル) + 上記全部 |

**意味**: 上位crate (db/ai/image/embedding/tauri) の**Rustレベルの
クロス欠陥は本環境では検出できない** (依存build scriptが先に失敗する
ため本体のcheckまで到達しない)。darwin実機 (またはクロスCツールチェーン
導入) での `cargo check` がWP-18 RC前の残課題。逆に、緑3 crateに
対しては「Linuxでは発火しないdarwin向けコンパイル欠陥が無いこと」が
保証されている。

### 1.2 windows (x86_64-pc-windows-msvc)

- `libsqlite3-sys` (cc→`lib.exe`要件)・dav1d等のC依存のため本環境では
  cross-check不可能。mingw gccも無し。
- **Windowsの検証手段は「検査+純関数テスト」のみ**とする制約を本書で
  明文化する。字句判定ロジックは`is_lexically_beneath`
  (gento-core/path_safety.rs。cfg外の純関数) として切り出し、
  `\\?\C:\` 形式を含むケースをLinux上のユニットテストで実証済み。

## 2. Windows既知制限

1. **`open_regular_beneath` / `OutputDir` は字句フォールバック**:
   `symlink_metadata` 成分歩行 + `canonicalize` 結果の
   `is_lexically_beneath` 包含検証 (§47対応表参照)。
   **TOCTOU耐性は無くbest-effort** — unix実装 (openat2 / O_NOFOLLOW
   fd歩行) の構造的保証と同等ではない。検査とopenの間の中間成分
   差し替えを検出できる保証は無い。
2. **`is_lexically_beneath` の字句判定限界**: 8.3短縮名
   (`PROGRA~1`)、末尾ドット/空白のWin32正規化、NTFS alternate data
   stream (`file.png:stream`) は解釈しない。`..` は字句解決
   (ルート以上はclamp)。
3. **OSファイルIDは常時 `None`** (`gento_platform::os_file_id_of`):
   Windows file ID (NTFS FILE_ID_128 / ReFS) の提供は未実装。
   衝突トークンはsize+mtimeで機能するが同一性検証は弱い
   (WP-18以降の課題)。
4. **パスのDB符号化はUTF-8固定** (`gento-db/src/paths.rs`
   `encode_path`): 非UTF-8表現のWindowsパス (ワイルドカード・
   予約名等を含む一部レガシー表現) はlossy変換される。
   ネイティブUTF-16保持はDB schema変更を要するため未対応。
5. **平文鍵フォールバックにACL付与が無い** (`gento-ai/src/apikey.rs`
   非unix分岐): keyringが実質常に利用可能なため到達しにくいが、
   フォールバック時のファイル保護は「書き込みのみ」。
   Windows ACL (所有者限定) 付与は未検証・未実装。
6. **`dir_free_bytes` は常時 `None`** (確認skip・fail-open)。
   `GetDiskFreeSpaceExW` 実装は未対応。
7. ゴミ箱・資格情報ストア (keyring)・外部アプリ起動 (opener plugin)・
   WebView2挙動はWindows実機での未検証 (OS別挙動の実装裏付けは§7.4/§7.5、
   利用者検証手順は§8チェックリスト)。

## 3. macOS既知制限

1. **プロトコルのキャッシュ系readが`/proc`依存で失敗する** (実行時
   欠陥・`apps/desktop/src-tauri/src/protocol.rs` `read_file_checked`):
   fd実パス再確認に `/proc/self/fd/{fd}` のreadlinkを使うため、
   macOSでは**サムネイル等キャッシュ系の配信が常に`ReadError::Io**
   で失敗する** (元画像の直接配信は`canonical_root=None`のため影響
   無し)。darwin修正方針: `fcntl(fd, F_GETPATH, buf)` で実パス取得
   (darwin専用分岐)。本環境でgento-tauriのdarwin checkが不可能なため
   **未修正** — 実機検証時に対応すること (unsafeコードをコンパイル
   検証なしに書くことは避けた)。
2. **RSS計測が`/proc`依存で常時None** (`gento-image/src/metrics.rs`
   VmRSS)。macOSでは`mach_task_info`等が必要。計測が無くなるだけで
   機能影響は無い。
3. **ゴミ箱undo情報の記録がXDG仕様依存**: `trash_to_bin` のエントリ
   特定はfreedesktop Trash仕様の`.trashinfo`新規出現比較で行う。
   trash crate自体はmacOS (~/.Trash) に対応するが、**trashinfoが無い
   ためエントリ特定が失敗し、trash操作がエラーになる**
   (undo情報の欠落を黙認しない設計のため)。macOS正式対応時は
   Finder Trash向けの特定手法 (移動後の~/.Trash差分等) が必要。
4. **UID取得が`/proc/self/status`依存** (`gento-platform`
   `current_uid`): macOSでは常に`None`となりadmin trash探索が
   行われない — 上記3と同じくXDG仕様側の問題で、macOSのゴミ箱
   経路では影響しない (ホームtrashのみ)。
5. **openat2不存在**: `open_regular_beneath` は`O_NOFOLLOW` fd歩行に
   フォールバックする (構造的保護は維持 — §47対応表参照。
   テスト`intermediate_symlink_directory_is_not_traversed`が公開API
   と歩行関数の双方を検証しており、macOS経路の接続確認済み)。
6. **空き容量確認が有効化** (WP-17-1): モデルDL (gento-embedding) の
   `available_bytes` は旧実装ではLinux限定だったが、
   `gento_platform::dir_free_bytes` (unix全体のstatvfs) 集約により
   macOSでも動作する (darwin向けコンパイルは1.1表の通り検証済み)。

## 4. platform差の隔離状況 (§25受け入れ条件)

### 4.1 gento-platformへ集約済み (WP-7 + WP-17-1)

| 機能 | 正本 | 備考 |
|------|------|------|
| OSゴミ箱 (XDG) | `gento_platform::trash_to_bin` / `restore_from_trash` | WP-7 |
| OSファイルID (unix dev+ino) | `gento_platform::os_file_id_of` | WP-7。fileops.rs・backup.rs・metadata.rsの計4箇所の散在実装をWP-17-1で集約 |
| 原子的no-clobber rename | `gento_platform::rename_noreplace` | WP-7 (renameat2 / link+unlink) |
| mtime (ns) | `gento_platform::mtime_ns_of` | **WP-17-1移動** — tauri fileops.rs (本体外+テストStubFs)・backup.rsの散在3実装を集約 |
| 空き容量 | `gento_platform::dir_free_bytes` | **WP-17-1移動** — tauri fileops.rs (unix statvfs) とgento-embedding download.rs (linux限定) を集約しunix全体へ拡大 |

### 4.2 移動しなかったもの (理由付き)

- **`path_safety.rs` のcfg群** (gento-core内): `open_regular_beneath`
  /`OutputDir` はcoreの公共API面そのものであり、platformへ移動すると
  core↔platformの依存方向が成立しない (platform→coreの一方向のため)。
  本体は「プラットフォーム抽象の実装」であり散在ではない。
- **パス符号化** (`gento-db/src/paths.rs` encode/decode): DB保存形式
  (正準表現) と結合しており、移動はDB schema議論を伴う。
  Windows UTF-16保持は§2.4の既知制限として記録。
- **資格情報ストア** (`gento-ai/src/apikey.rs`): keyring依存と
  プロファイル管理が一体化した構造で、抽出はリスクに見合わない。
  ACL等の未検証項目は§2.5に記録。
- **外部アプリ起動**: 既に単一choke point (`tauri-plugin-opener`,
  `fsview.rs`) に隔離されており追加作業不要。
- **改行正規化**: 存在しない (エクスポートの.txtは常にLF)。
  将来CRLF対応が必要になった場合はplatformへ`line_ending()`を追加する。
- **`statvfs`・`/proc`系の残存** (`metrics.rs` VmRSS、`protocol.rs`
  fd実パス、fsviewテスト用root判定): いずれも単一箇所で使用され
  共有されるプラットフォーム差ではない。macOS影響は§3に記録。

## 5. WP-17-1で修正したクロスプラットフォーム欠陥

1. **E0617** (`crates/gento-core/src/path_safety.rs`
   `create_exclusive_at`): darwinの`libc::mode_t`は`u16`で、
   variadicな`openat`のmode引数に渡せない。`as libc::c_uint`へ修正
   (Linuxの`mode_t`=`u32`=`c_uint`と一致するため両platformで正しい)。
2. **core テスト`TestFs::mtime_ns_of`**: 秒→ナノ秒変換
   (`*1_000_000_000`) が欠落し「秒+ナノ秒」をns値として返していた
   (テスト専用ヘルパー。表示・比較の一貫性のため修正)。

## 6. Linux配布物とWebKitGTK (WP-17-2 §25 Linux受け入れ)

### 6.1 配布形式3種の成立

`apps/desktop/src-tauri/tauri.conf.json` の `bundle.targets` は
`["deb", "rpm", "appimage"]`。ビルドコマンドと生成物 (2026-09-04時点):

```sh
pnpm --dir apps/desktop tauri build
```

| 形式 | path | サイズ | sha256 (先頭12) |
|------|------|--------|------------------|
| deb | `target/release/bundle/deb/GEN-TO Spike_0.1.0_amd64.deb` | 23,644,050 B | `e7584eff5fbf` |
| rpm | `target/release/bundle/rpm/GEN-TO Spike-0.1.0-1.x86_64.rpm` | 23,645,067 B | `ca4b2e77a246` |
| AppImage | `target/release/bundle/appimage/GEN-TO Spike_0.1.0_amd64.AppImage` | 97,999,352 B | `6904b1ed92ff` |

- **deb** (`dpkg-deb -I`/`-c` 検証): `Depends: libwebkit2gtk-4.1-0,
  libgtk-3-0`。内容は `/usr/bin/gento-tauri` (68MB) + `.desktop`
  (hicolor 32/128/256@2xアイコン)。**システムのWebKitGTK/GTKに動的
  リンク**する。
- **rpm** (ヘッダ解析で検証): `Requires: libwebkit2gtk-4.1.so.0()(64bit),
  libgtk-3.so.0()(64bit)`。ファイル構成はdebと同一。
- **AppImage**: linuxdeploy (GTK plugin) により**GTK3・WebKitGTK等の
  ランタイム一式を同梱** (そのため約98MB。debの4倍)。生成には
  linuxdeploy/AppRunのダウンロード (ネットワーク) が必要 — 初回のみ
  `~/.cache/tauri` へキャッシュ。FUSE不在環境では
  `--appimage-extract-and-run` で起動可。

**動作方式**: フロントエンド資産 (dist/) は**バイナリへ埋め込み**
(tauri `custom-protocol`。tauri.conf.jsonに`resources`/`externalBin`定義
なし)。埋め込みモデル (ruri3 ONNX) は配布物に含まれず、実行時に
`--models-dir`/`GENTO_MODELS_DIR`/既定のローカルdirへDL・検証
(gento-embedding download.rs)。DB・サムネイルキャッシュもユーザー領域
(`--db`/`--cache-dir`で上書き可)。

### 6.2 実機起動確認 (Xephyr :6・2026-09-04)

- **deb**: `dpkg-deb -x` で展開した `/usr/bin/gento-tauri` を
  `--dataset sample/dataset` (1,615件) で起動 → adhoc走査
  (`scan metrics found=1615 errors=0`)・サムネイルグリッド描画を
  スクリーンショットで確認 → SIGTERMでgraceful終了
  (`shutdown wal checkpoint complete`)。
- **AppImage**: 同条件で起動 → グリッド描画確認 → graceful終了。
- 証拠: `/tmp/opencode/wp17/bundle-verify/` (logs・スクリーンショット。
  WP-17-2ハンドオフにも抜粋)。

### 6.3 WebKitGTK依存と最低バージョン

- WebViewは **webkit2gtk-4.1 API** (GTK3版)。依存関係は
  wry 0.55.1 (tauri 2.11.5 runtime) 経由。
- **最低バージョン: WebKitGTK 2.40** (wry 0.55.1 README
  "WebKit2GTK v2.40 or above")。Ubuntu 22.04 (2.40.x) が下限目安、
  検証環境は Ubuntu 24.04 + **2.52.6** (`libwebkit2gtk-4.1-dev`)。
- `ldd target/release/gento-tauri`: `not found` 0件。主要動的依存:
  `libwebkit2gtk-4.1`・`libjavascriptcoregtk-4.1`・`libgtk-3`・
  `libsoup-3.0`・gstreamer一式 (`libgst{app,gl,video,...}`)・
  `libsecret-1` (keyring/Secret Service)・`libsqlite3`・`libdav1d`
  (AVIF)・`libwayland-{client,cursor,egl}` + `libX11` (両プロトコル
  対応)。

### 6.4 X11主要操作確認 (WP-17-2時点でグリーン)

release devtoolsビルド (`cargo build --release -p gento-tauri
--features custom-protocol,devtools`) をXephyr :6で検証:

- **uxlib1 E2E**: 18/18条件 PASS・FAILURES=0
  (登録→一覧→画像表示 (DOM+ピクセル実測) →編集 (★5) →再起動→
  adhoc理由表示→表示切替→SIGTERM graceful終了)。
- **qa_wp4 E2E**: 11項目 PASS (走査中フォルダ切替・キャンセル・
  adhocセッション再開・件数ずれ警告)。
- ビルド成果物 (deb/AppImage) 自体のdevtest E2Eは、配布ビルドが
  dev bridge (devtools feature) を含まないため実施不可 — 成果物は
  §6.2の起動+描画確認、操作系は同一コードのdevtools releaseビルドで
  検証と区分する。

### 6.5 Wayland

本環境はX11セッション (`XDG_SESSION_TYPE=x11`) で、Wayland
コンポジタ (weston/sway) が不在のため**実機確認は未実施**。
バイナリはGTK3 Waylandバックエンド対応 (§6.3のlddで
libwayland系を確認済み)。

**利用者手順 (Wayland環境での確認方法)**:

```sh
GDK_BACKEND=wayland ./gento-tauri   # deb/rpmインストールバイナリ
GDK_BACKEND=wayland ./GEN-TO*.AppImage
```

確認観点: ①一覧グリッドのサムネイル表示 ②ビューア (Enter)・
クイックプレビュー (Space) ③2枚/4枚比較 ④`gento-original:`等
ローカルプロトコル配信 ⑤日本語IME ⑥ドラッグ&ドロップ ⑦分数
スケール (125%/150%) での文字・グリッド崩れ。X11と異なる既知の
一般論として、WebKitGTKのWaylandバックエンドはX11より問題が
残りやすい (バージョン2.40-2.44の範囲でDnDやIMEの既知不具合報告
あり) — 問題時は `GDK_BACKEND=x11` への切替で回避可能。

## 7. 政策決定の記録 (WP-17-2 §24申し送りの決着)

### 7.1 100MP級画像の256MiBデコード保護上限 — 上限維持

- **決定**: WP-3の保護上限を維持する
  (`MAX_DECODE_ALLOC_BYTES` = 256MiBデコード割当・
  `MAX_DECODE_DIMENSION` = 1辺20,000px ≒ RGBA換算64MP)。
  100MP級はサムネ/preview生成対象外 (保護的拒否)。
- **挙動の確認** (テストで実証済み):
  `dimension_limit_blocks_decompression_bomb_header`・
  `jpeg_dimension_bomb_is_rejected_by_limits`・新規
  `limits_error_message_tells_the_user_the_reason_and_the_limits`
  (いずれも `crates/gento-image/src/thumbnail.rs`)。実測は
  baselines §4 (102MP PNGがIHDR予約段階で0.3ms拒否・UI継続)。
  拒否されたファイルは`thumb:failed`イベントで失敗セルとして
  記録され (再要求されず)、100MPの**表示**はoriginal
  (`gento-original:`配信) で可能。
- **利用者への伝達 (WP-17-2で改善)**: 上限拒否のエラー文言が
  「上限値と理由」を伝えるよう`ThumbnailError`のDisplayを改善
  — `decode failed: 画像が大きすぎるため上限で拒否しました
  (上限: デコード割り当て256MiB・1辺20000px): <詳細>`。UI側も
  失敗セル (✕) のtooltipに失敗理由を表示するよう変更
  (grid `FailureSet.reasonFor`・`failureReasonFor`)。
- 上限引き上げ (512MiB級) は必要時にWP-18以降で再議論。

### 7.2 AVIF probe 69ms — 維持 (コスト既知)

dav1dフルデコードでprobeが最大69ms/件 (WP-3設計)。ハッシュ
パイプラインに含まれるためAVIF比率が高いコレクションでは走査が
遅くなるが、実測259件/50kで影響軽微 (baselines §12.5)。
**コード変更なし・既知コストとして文書化のみ** (baselines §12.5に
政策決定記録を追記)。

### 7.3 macOSのMVP組み込み判断 — best-effort (正式対応外)

**MVP受け入れはLinux + Windows。macOSはbest-effortとする**
(正式対応とはしない)。根拠:

1. **実行時欠陥が未修正** (WP-17-1で発見・文書化):
   - プロトコルのキャッシュ系readが`/proc/self/fd`依存で常に失敗
     (§3.1 — サムネイル等の配信がmacOSで機能しない見込み)。
   - ゴミ箱undo情報の特定がXDG trashinfo依存で失敗 → trash操作が
     エラーになる (§3.3)。
2. **C依存のクロス検証不可**: gento-tauri等上位5 crateのdarwin
   checkが本環境で実行不能 (§1.1) — macOS向けRust欠陥の検出手段がない。
3. **実機不在**: ビルド・起動を含む一切の実機検証が未実施。

macOS正式対応はWP-18 RC以降の候補とし、§3.1の`fcntl(F_GETPATH)`
修正・§3.3のFinder Trash対応・実機build確認を必要条件とする。

### 7.4 資格情報ストア (API key) の現状

実装 (`crates/gento-ai/src/apikey.rs`) の実態:

- **保存先はOS資格情報ストアが第一** (keyring crate):
  Linux = Secret Service (D-Bus) / macOS = Keychain /
  Windows = Credential Manager。失敗時 (D-Bus不在等) のみ
  アプリデータdir直下 `ai-secrets/` の**平文ファイル**へフォールバック
  (0700ディレクトリ・0600ファイル・`_README.txt`で平文である旨を警告)。
- **設定DBには鍵本体を書かない**: `ai_profiles.api_key_ref` は
  参照文字列 (`keyring:<id>`/`file:<id>`) のみ。鍵がDB・通常ログへ
  出る経路は存在しない (テスト`api_keys_never_in_db`で保証)。
- **暗号化**: フォールバックファイルは平文 (意図的 — 鍵を同一
  マシンに置く疑似暗号化は安全性の実態を曖昧にするため)。
- **Windows/macOSのkeyring経路は実機未検証** (LinuxのSecret Service
  経路のみ動作確認済み)。Windows ACL付与 (§2.5) と合わせ、
  keyring実機確認は**RC以降の改善項目**。

### 7.5 ゴミ箱・外部アプリ起動のOS別挙動

いずれも単一choke pointに隔離済み (§4)。

**ゴミ箱** (`gento_platform::trash_to_bin` / trash crate 5.2.6 +
undo情報のエントリ特定は自前実装):

| OS | 移動先 | 実装 | undo情報特定 | 備考 |
|----|--------|------|--------------|------|
| Linux | XDG Trash (`~/.local/share/Trash`またはマウントポイント毎) | freedesktop Trash仕様 (trashinfo) | 新規trashinfoの`Path=`一致 | 検証済み (WP-7〜) |
| Windows | ごみ箱 (Recycle Bin) | Shell COM `IFileOperation` | **未検証** (trashinfo無し — macOSと同じ課題) | 実機検証は§9チェックリスト |
| macOS | `~/.Trash` | `NSFileManager.trashItemAtURL` | **失敗する見込み** (§3.3) | 正式対応外 (§7.3) |

**外部アプリ起動・ファイルマネージャー表示** (`fsview.rs` /
tauri-plugin-opener 2.5.4 — JS側commandは不使用):

| 操作 | Linux | Windows | macOS |
|------|-------|---------|-------|
| 既定アプリで開く (`open_path`) | `xdg-open` | `ShellExecuteW` (既定ハンドラ) | `/usr/bin/open` (LaunchServices) |
| ファイルマネージャーで表示 (`reveal_item_in_dir`) | DBus `org.freedesktop.FileManager1.ShowItems` (fallback: xdg-desktop-portal OpenDirectory) | `SHOpenFolderAndSelectItems` (Explorerで選択) | `NSWorkspace.activateFileViewerSelectingURLs` (Finderで選択) |

Linux経路はWP-4以降で実使用済み。Windows/macOSは実機未検証
(§9チェックリストに含む)。

## 8. Windows利用者手動検証チェックリスト (MVP受け入れ用)

対象: Windows 10/11 x64。開発環境でWindows実機検証ができないため、
MVP受け入れ (計画書§25「LinuxとWindowsで主要MVPフローを完了できる」)
に向けて**利用者が実行するチェックリスト**として整備する。

### 8.1 事前要件

- **WebView2 Evergreen Runtime** (Tauri v2 Windows要件。
  Windows 11は標準搭載、Windows 10は多くの場合既存)。
  不在なら https://developer.microsoft.com/microsoft-edge/webview2/
  のEvergreen Bootstrapperで導入。
- インストーラ: `pnpm --dir apps/desktop tauri build` のWindows
  実行でNSIS (`.exe`) / MSI (`.msi`) を生成 (tauri.conf.json
  bundle.targetsに`nsis`, `msi`追加が必要 — Linux側では未設定)。
- **署名はMVPでは自己署名 (無署名) で可**: SmartScreen警告は
  「詳細情報→実行」で迂回。正式署名はリリース工程 (WP-18以降)。
- アンインストール時のデータ保持: アプリデータdir
  (`%APPDATA%/dev.gento.spike/`) はアンインストーラで削除しない
  設定 (Tauri既定) — 要確認。

### 8.2 主要フローチェックリスト

1. **初期設定**: 初回起動 → LibraryPanelに画像フォルダを登録 →
   追加 → ライブラリ表示へ自動切替。
2. **登録 (走査)**: ファイル追加・リネーム・削除が一覧へ反映
   (フォルダ監視)。1,000件級で走査完了。
3. **一覧**: グリッド表示・スクロール・サムネイル生成・サイズ切替
   (単一/2枚/4枚比較への切替)。
4. **ビューア**: Enter→ビューア・Space→クイックプレビュー・
   ズーム (オリジナル解像度表示)。
5. **比較**: 2枚/4枚比較の選択・切替。
6. **評価**: ★評価・選別 (keep/reject)・お気に入り・タグ付け。
7. **エクスポート**: 選択画像のエクスポート (コピー) → 内容一致。
8. **バックアップ**: DBバックアップ作成 → 別dirへ保存。
9. **復元**: バックアップからの復元 → 一覧・評価が復元される。
10. **ごみ箱**: ファイル→ごみ箱 (§7.5: Recycle Bin経路) +
    元に戻す。**要重視** (trashinfo無し環境のundo特定が未検証)。
11. **外部アプリ**: 右クリック→「フォルダーで表示」(Explorerで選択)
    ・「既定のアプリで開く」。
12. **AI設定**: API key登録→keyring (Credential Manager) に保存
    されるか (資格情報マネージャーで`gento`エントリ確認)。

### 8.3 Windows特有の確認

- **長いパス** (MAX_PATH超): `C:\...260文字超`の画像フォルダ登録・
  表示・エクスポート (字句フォールバック§2.1の実地確認)。
- **ドライブ境界**: 別ドライブ (D:等) へのエクスポート・移動。
- **Windows file ID**: `os_file_id_of` が非unixで`None` (§2.3) —
  同一トークン判定がsize+mtimeで機能すること (リネーム後も評価が
  追従するか)。
- **UNICODEパス**: 日本語・絵文字を含むパス/ファイル名の登録・
  表示・リネーム・エクスポート (DB符号化はUTF-8 lossy §2.4)。
- **WebView2**: 上記全操作がWebView2 Evergreen (Edge最新) で成立。
  WebView2 Runtime固定バージョンでの検証は不要 (Evergreen前提)。

## 9. macOSビルド・検証手順 (best-effort — 正式対応外 §7.3)

- **buildコマンド** (実機macOS・aarch64):
  `pnpm --dir apps/desktop tauri build` (bundle.targetsに`dmg`/
  `app`追加が必要 — Linux側では未設定)。
- 依存: Xcode Command Line Tools・WKWebView (システム提供。
  追加ランタイム要件なし)。
- **sandboxは未検証** (アプリ署名・notarization・entitlementsが
  未整備 — spike段階では開発者ローカル実行のみ想定)。
- 既知の実行時欠陥 (§3.1プロトコルキャッシュread・§3.3ゴミ箱undo)
  はmacOS正式対応時に修正が必要。build成功のみでは「動作する」を
  意味しない点に注意。

## 10. WP-17-2で実施した検証の証拠一覧

| 検証 | 手段 | 結果 |
|------|------|------|
| bundle 3形式生成 | `pnpm --dir apps/desktop tauri build` | §6.1表の通り |
| deb構成検証 | `dpkg-deb -I`/`-c` | Depends・ファイル構成を記録 |
| rpm構成検証 | 純Pythonヘッダ解析 (rpmコマンド不在のため) | Requires・ファイル一覧を記録 |
| deb/AppImage起動 | Xephyr :6 + スクリーンショット | グリッド描画・graceful終了を確認 |
| 動的依存 | `ldd target/release/gento-tauri` | not found 0件・§6.3に一覧 |
| X11主要操作 | uxlib1_e2e.sh (Xephyr :6) | 18/18 PASS |
| X11走査・セッション | qa_wp4_e2e.sh (Xephyr :6) | 11項目 PASS |
| Wayland | 環境確認 (`XDG_SESSION_TYPE=x11`・compositor不在) | 実施不可 → §6.5に利用者手順 |
