# プラットフォーム別既知制限と検証状況 (WP-17-1)

最終更新: 2026-09-04

GEN-TO MVPの正規開発・検証環境はLinux (x86_64) のみ。本書は
WP-17-1時点で判明しているプラットフォーム差・制限・検証カバレッジの
正本である。計画書§25 (WP-17) の受け入れ条件
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
   WebView2挙動はWindows実機での未検証 (MVP検証はWP-17実機フェーズ)。

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
