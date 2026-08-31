# 依存ライブラリメモ (Phase 2 / WP-2時点)

- 記録日: 2026-08-28 (Phase 1 → WP-2更新 → WP-3更新 2026-08-29 → WP-4更新 2026-08-29 → WP-5更新 2026-08-29 → WP-0確認 2026-08-30 → AVIF-1更新 2026-08-31 → WP-5 (MVP) 更新 2026-08-31)
- 方針: バージョンはpin (npmは正確なバージョン指定、Cargoは `=` 付き + Cargo.lock)。追加・変更時に本メモへ記録する。

## Rust (workspace直依存)

| crate | pinバージョン | 用途 | ライセンス | 使用crate |
|---|---|---|---|---|
| serde | =1.0.229 (features: derive) | DTOのSerialize/Deserialize | MIT OR Apache-2.0 | gento-core, gento-tauri |
| serde_json | =1.0.151 | gento-core単体テストのJSON形状検証 (通常依存には入れない) | MIT OR Apache-2.0 | gento-core (dev), gento-tauri (dev) |
| tauri | =2.11.5 | デスクトップシェル・IPC・WebView | Apache-2.0 OR MIT | gento-tauri |
| tauri-build | =2.6.3 | build.rsでのアセット/スキーマ生成 | Apache-2.0 OR MIT | gento-tauri (build) |
| tracing | =0.1.44 | 構造化ログ (走査計測ログを含む) | MIT | gento-tauri, gento-image |
| tracing-subscriber | =0.3.23 (features: env-filter) | ローグ出力実装 (`GENTO_LOG`環境変数対応) | MIT | gento-tauri |
| walkdir | =2.5.0 | ディレクトリ再帰走査 (WP-2。エントリ列挙・エラー隔離) | Unlicense OR MIT | gento-image |
| png | =0.18.1 | PNGシグネチャ+IHDRのみの寸法プローブ (WP-2。全文デコードなし) | MIT OR Apache-2.0 | gento-image |
| image | =0.25.10 (default-features=false, features: png, jpeg, gif, bmp, tiff, webp, avif-native) | サムネイル用デコード・リサイズ (WP-3 + AVIF-1)。PNGのみ→6形式→AVIF追加と攻撃面を段階的に拡大。`Limits` (max_alloc 256MiB・寸法20000) でdecompression bomb対策。`DynamicImage::thumbnail_exact` で拡大なしリサイズ | MIT OR Apache-2.0 | gento-image, gento-tauri (dev: テストPNG生成) |
| webp | =0.3.1 (default-features=false) | lossy WebPエンコード (quality 80、`Encoder::from_rgba` への明示的RGBA渡し) およびテストでの往復デコード (`Decoder`。feature不問で利用可)。`img` featureは無効化し image crate との型結合に依存しない (PM計画 §3)。libwebp-sys はccでビルド (BSD-3-Clause) | MIT OR Apache-2.0 | gento-image |
| twox-hash | =2.1.4 | キャッシュキーのXxh3 128bit (`xxhash3_128::Hasher::oneshot`)。非暗号だが128bitで衝突耐性を確保し、キー入力 (パス+size+mtime+クラス+バージョン) を決定的に圧縮 | MIT | gento-image |
| dirs | =6.0.0 | 既定キャッシュディレクトリ解決 (`cache_dir()`) | MIT OR Apache-2.0 | gento-tauri |

## TypeScript / Svelte (apps/desktop)

| パッケージ | pinバージョン | 用途 | ライセンス |
|---|---|---|---|
| @tauri-apps/api | 2.11.1 | invoke等のIPCクライアント | Apache-2.0 OR MIT |
| @tanstack/svelte-virtual | 3.13.36 | 仮想スクロール (WP-5)。内部で `@tanstack/virtual-core` 3.17.8 を使用 | MIT |
| svelte | 5.56.10 | UIフレームワーク (runes使用) | MIT |
| @tauri-apps/cli | 2.11.4 | `pnpm tauri` (dev/build) | Apache-2.0 OR MIT |
| vite | 8.2.2 | devサーバー・bundler | MIT |
| @sveltejs/vite-plugin-svelte | 7.3.0 | Vite向けSvelteコンパイル | MIT |
| typescript | 6.0.3 | 型検査。7系はsvelte-check (peer `^5||^6`) とtypescript-eslint (peer `<6.1.0`) が未対応のため6系最新をpin | Apache-2.0 |
| svelte-check | 4.7.6 | .svelte/.ts型検査 (`pnpm check`) | MIT |
| vitest | 4.1.11 | フロント単体テスト (`pnpm test`) | MIT |
| eslint | 10.9.1 | Lint (`pnpm lint`) | MIT |
| @eslint/js | 10.0.1 | eslint推奨ルールセット | MIT |
| eslint-plugin-svelte | 3.23.0 | .svelteファイルのLint | MIT |
| typescript-eslint | 8.68.0 | TS用eslintルール (非type-checkedのrecommendedのみ) | MIT |
| globals | 17.11.0 | ブラウザglobals定義 | MIT |
| esbuild (間接) | (vite依存) | postinstallビルド許可のためpnpm `onlyBuiltDependencies` に明記 | MIT |

## 選定理由

- **`pnpm tauri` + `@tauri-apps/cli` devDependency**: cargo-tauriサブコマンドをグローバル導入しない。プロジェクトローカルにバージョンをpinでき、`pnpm dlx`/`npx`のような解決時のバージョン漂動がない。
- **eslint最小構成**: js.recommended + typescript-eslint recommended (非type-checked) + eslint-plugin-svelte flat/recommended。type-checked構成はCI時間と設定複雑度に対しスパイクの利益が小さいため入れない。追加は必要になった時点で検討。
- **prettier不導入**: Svelte/TSは `svelte-check` + `eslint` で品質を担保し、フォーマットはRust側 `cargo fmt` と異なりチーム規約がまだないため。フォーマッタが必要になった時点で導入を検討する。
- **TypeScript 6.0.3**: 最新安定7系 (native port) はsvelte-check・typescript-eslintの両方が未対応。6系最新をpinし、対応状況が変わった時点で更新する。
- **Node 25 (非LTS) 注意**: 開発機のNodeはv25.8.2。vite 8・vitest 4は動作確認済みだが、問題発生時はLTS (22系) でも検証する (PM計画 §7-11)。
- **serde手動implでAssetIdを文字列化**: serde_with等の追加依存なしでJS安全整数問題 (u64 > 2^53-1) を回避する。
- **gento-coreのserde_jsonはdev-dependency**: コアはJSON実行時依存を持たず、シリアライズ形状のテストのみに使う。
- **gento-tauriのlib crate-typeはrlibのみ**: 公式テンプレートのstaticlib/cdylibはmobile対象。desktop専用スパイクではビルド時間増大だけが残るため除外した。

## WP-2で追加した依存の選定理由

- **`walkdir` =2.5.0**: 定番の再帰ディレクトリ走査。イテレータが `Result` を返すため、権限不足等を
  「1項目分のエラー」として収集して走査継続する要件 (本体計画 §6「エラーはファイル単位で隔離」) に
  自然に対応できる。デフォルトでシンボリックリンクを辿らないため安全側。
- **`png` =0.18.1**: IHDR読み取りのみで幅・高さが取れる (`Decoder::read_header_info`)。
  docs.rs 0.18.1 で確認した使い方: `Decoder::new_with_limits(BufReader::new(File), Limits { bytes })`
  — 0.18は `R: BufRead + Seek` が必要なため `BufReader` で包む。`Limits` を1MiBに設定し安全弁とする。
  エラー判定: `DecodingError::IoError` のうち `UnexpectedEof` は切り詰め (=Corrupt)、それ以外の
  IoErrorはUnreadable、Format/LimitsExceededはCorruptとして分類。
- **tracingをgento-imageでも使用**: 走査計測ログ (`scan metrics` 行、elapsed + RSS) の出力のため。
  実行時の新規依存ではなくworkspace既存crateの利用範囲拡大。

## WP-3で追加した依存の選定理由

- **`image` =0.25.10 (pngのみ)**: デコード+リサイズの標準crate。`default-features=false`
  + `features=["png"]` でPNG codecのみ (攻撃面・ビルド時間を最小化、PM計画 §3)。
  リサイズは `DynamicImage::thumbnail_exact` (高速な整数box平均。指定寸法を正確に生成)。
  デコード制限は `image::Limits` の `max_image_width/height=20000` (strict) と
  `max_alloc=256MiB` (データセット最大7.9MP RGBA=32MiBに対し8倍の安全域)。
- **`webp` =0.3.1 (default-features=false)**: libwebp-sysの安全ラッパー。
  `Encoder::from_rgba(buf, w, h).encode(80.0)` でlossy品質80。`default-features=false`
  (`img` feature無効) により `image` crateとのtrait結合に依存せず、明示的RGBA
  バッファを渡す (PM計画 §3の契約)。デコード (`Decoder`/`WebPImage`/`BitstreamFeatures`)
  はfeature不問で利用可能なため、透過往復テストにも同じcrateを使う。
- **`twox-hash` =2.1.4**: キャッシュキーハッシュ。Xxh3は非暗号ながら高速で、
  128bit出力 (`xxhash3_128::Hasher::oneshot`) により実用上の衝突を回避
  (32hex文字をファイル名に埋め込み)。暗号強度が必要になったら内容ハッシュ移行
  (AssetFingerprint関数境界) で差し替える。
- **`dirs` =6.0.0**: XDG規約に沿ったキャッシュディレクトリ解決のみに使用。
  失敗時は `std::env::temp_dir` へフォールバック。
- **gento-coreをgento-imageが依存**: §5.1「ジョブ優先度の基本型はcore」に従い
  `Priority`/`SizeClass` を `gento_core::jobs` へ置いたため (内部依存追加)。

## WP-4で追加した依存の選定理由

- **追加なし (Rust/TS両方)**: ローカル画像プロトコルのURL構造化パースには
  `tauri::Url` (tauriが再エクスポートする `url::Url` 2.5.8、lock済みの推移依存) を使用。
  スキーム登録 (`register_asynchronous_uri_scheme_protocol`) とHTTP応答組み立て
  (`tauri::http`) も既存tauri部品のため、`url`/`http` を直依存に加える必要がない
  (PM計画「依存追加は最小に」)。percentile算出は `gento_image::stats::percentile` を再利用。
- **CSP設定のみ追加** (`tauri.conf.json`): `img-src 'self' data: gento-thumb:
  gento-preview: gento-original:`。`csp` と `devCsp` で同一値。

## WP-5で追加した依存の選定理由 (判断ゲートAの記録)

- **`@tanstack/svelte-virtual` =3.13.36 を採用 (ゲートA第一候補のまま)**:
  - 検証内容: (1) peerDependenciesが `svelte ^3.48 || ^4 || ^5` とSvelte 5を明示サポート、
    (2) dist実装は `svelte/store` の `writable`/`derived` で `Virtualizer` インスタンスを
    ラップするだけの薄いアダプタ (52行) であり、Svelte 5 runesコンポーネントから
    明示的 `subscribe` で利用可能、(3) `pnpm check` / `pnpm build` / 単体テストが
    すべて通過、(4) 実アプリで5万件カタログの行単位仮想化・overscan切替・
    scrollToIndex追従・スクロール位置復元が期待どおり動作し、DOMセル数が
    可視行+overscanに厳密に比例 (49/63/35/21/39セルを幾何計算とOCRで確認)。
    機能・性能とも問題なしと判断し、virtual-core直使用や命令的DOM更新
    (ゲート2/3) への降格は不要とした。
  - 既知の留意点: アダプタの `setOptions` は `estimateSize` 変更を測定キャッシュへ
    反映しない (virtual-coreの `getMeasurements` メモ化がestimateSizeを依存に持たない
    ため)。セルサイズ切替は `{#key cellClass}` でコンポーネントごと再生成して対応
    (データはGridStore側に残るため再取得不要)。
    また `useAnimationFrameWithResizeObserver: true` を指定し、ResizeObserverの
    同一フレーム再通知によるloop警告を抑制している。

## WP-5の実装上の発見 (依存ではないが再利用時の注意)

- Tauri IPCの応答はまれに喪失しうる (WebViewのフォーカス遷移中に観測)。
  `get_catalog_page` / `request_thumbnails` のpromiseが未解決のまま残ると、
  ページ取得のin-flight dedupとサムネイルpending集合が永久に停滞する。
  WP-5では両invokeに8秒タイムアウトを付けてエラー経路へ落とし、次の
  レンジ変化で再試行する設計とした (`grid/pages.ts` / `grid/grid-store.ts`)。

## 将来WPでの追加予定

- (WP-5で追加予定だった `@tanstack/svelte-virtual` は上記のとおり採用済み)

## WP-4 (MVP) で追加した依存 (2026-08-30。製品UIシェルと通常ファイラー)

- **`tauri-plugin-opener` =2.5.4** (Apache-2.0 OR MIT、tauri公式plugin):
  OSファイルマネージャーで表示 (reveal) と外部アプリで開く (open) に使用。
  選定理由: (1) reveal項目選択をLinux ( freedesktop FileManager1 D-Bus →
  `xdg-open <親>`フォールバック)・Windows (explorer /select)・macOS (NSWorkspace)
  で正しく実装済みの公式実装を再利用し、自前の`std::process::Command`群を
  作らない (Windows/macOS対応はWP-17で実機確認)。要件のtauri ^2.10は
  workspaceのtauri 2.11.5と適合。
  (2) **Rust APIのみを使用** (`reveal_item_in_dir` / `open_path` を独自のthin
  commandから呼ぶ)。JS側から `plugin:opener|*` commandを叩かないため
  capability (default.json) への権限追加が不要で、「capability最小化」原則を
  维持する。パスの存在検証・正規化は独自command側で行う。
- **フロントエンドの新規npm依存なし**: クリップボードは
  `navigator.clipboard.writeText` + `document.execCommand("copy")` フォールバック
  の最小実装 (tauri-plugin-clipboard-managerを入れない)。ペイン状態・
  選別セッションの永続化は既存localStorage。uuidは`crypto.randomUUID()`。

## WP-0での依存変更 (2026-08-30)

- **追加なし (Rust/TS両方)**。devtools feature隔離・AppDirs・panic hook・
  APIバージョンはいずれも既存依存 (dirs / tracing / serde) の範囲で実装した。
  ライセンス一覧の生成手段を `scripts/gen-licenses.sh` として追加
  (成果物: [licenses.md](licenses.md))。

## WP-1で追加した依存 (2026-08-30。SQLite基盤)

- **`rusqlite` =0.31.0 (features: `bundled`, `backup`)** (gento-db, 通常依存)
  - 用途: SQLiteアクセス (WAL・foreign_keys・busy_timeout・migration)。
    `backup` featureはSQLite Backup API用に有効化 (migration前backupの実装選択肢。
    実際のbackupは `VACUUM INTO` を使用しているが、接続間copyの正規手段として feature確保)。
  - 入手元: crates.io (`rusqlite`。wrappers/rusqliteリポジトリの派生版)。
  - ライセンス: **MIT OR Apache-2.0** (Rustラッパー部分)。
  - 同梱SQLiteのlicense表記: `libsqlite3-sys` (下記) のbundledビルドがリンクする
    **SQLite amalgamationはパブリックドメイン** (https://sqlite.org/copyright.html)。
    再配布義務・NOTICE要件なし (帰属表示の義務も法的には無いが、
    licenses.mdへ明記して帰属を保持する)。
  - 依存するnative library: **なし** (システムSQLiteにリンクしない bundledビルド。
    `cc` crateで同梱 `sqlite3.c` をコンパイル。openssl等の外部ライブラリ不使用)。
  - 対応プラットフォーム: Linux / macOS / Windows (ccでコンパイルできる環境すべて。
    WebView環境非依存のためCLI/workerからも使用可)。
  - **FTS5**: bundledビルドのコンパイルフラグに `SQLITE_ENABLE_FTS5` が含まれる
    (libsqlite3-sys 0.28.0 build.rsで確認。実行時可用性テストもあり:
    `gento-db` のFTS5テスト)。
  - バージョンpin理由: 0.32以降で `Connection` APIの一部が変わるため、
    動作検証済みの0.31系をpin (Cargo.lockで完全固定)。
- **`libsqlite3-sys` =0.28.0 (rusqlite bundled経由の推移依存)**
  - ライセンス: MIT OR Apache-2.0 (build.rs・バインディング部分) +
    SQLite amalgamation (パブリックドメイン)。
  - native依存: `cc` (MIT OR Apache-2.0) のみ。bindgen/clang不要
    (事前生成bindings使用)。
- **`uuid` =1.26.0 (features: `v7`)** (gento-db, 通常依存)
  - 用途: UUIDv7生成 (36文字小文字TEXT。PM計画§6-4)。
    一意性のみ保証 (単調性は保証しない——crate docに制約明記)。
  - ライセンス: MIT OR Apache-2.0。
  - 推移依存: `getrandom` (MIT OR Apache-2.0。OS乱数取得のみでnative libなし)、
    `zerocopy` + `zerocopy-derive` (MIT OR Apache-2.0)。
  - 対応プラットフォーム: 全主要OS (getrandomが各OSのシステム乱数APIを使用)。

## WP-2 (MVP) で追加した依存 (2026-08-30。登録ルート・永続カタログ・差分スキャン)

- **`blake3` =1.8.7** (gento-image, 通常依存)
  - 用途: 画像内容ハッシュ (Asset同一性判定。data-model §2.1「バイト列ハッシュ
    一致 = 同一Asset」)。`Hasher::new()` + 1MiBチャンク `update` のストリーミング
    計算で大きなファイルもメモリ一定。形式は `blake3:<64hex小文字>`。
  - 入手元: crates.io (BLAKE3チーム公式crate。READMEの実装はRust)。
  - ライセンス: **CC0-1.0 OR Apache-2.0 OR Apache-2.0 WITH LLVM-exception**
    (アセンブリ部分がLLVM例外条項付きApache-2.0)。
  - 推移依存 (default featuresのみ使用): arrayvec (MIT OR Apache-2.0) /
    cfg-if (MIT OR Apache-2.0) / constant_time_eq (MIT OR Apache-2.0 OR Zlib) /
    cpufeatures (MIT OR Apache-2.0)。native libraryなし (pure Rust + SIMD)。
    rayon/mmap/serde featureは無効のまま。
  - 対応プラットフォーム: 全主要OS (純Rust実装)。
- **`notify` =8.2.0** (gento-tauri, 通常依存)
  - 用途: 登録ルートのフォルダ監視 (WP-2)。Linuxではinotify backend
    (`notify::recommended_watcher` = inotify)。`RecursiveMode::Recursive` で
    ルート配下を再帰監視。debounce/結合は自前実装 (gento-core
    `library::WatchBuffer`) — notify 8の`notify-debouncer`系は別crateのため
    取り込まず、結合規則をドメイン層でテスト可能にした。
  - 入手元: crates.io (notify-rs公式)。
  - ライセンス: **CC0-1.0**。notify-types (CC0-1.0) を含む。
  - 推移依存 (Linux): inotify 0.11 (ISC) / inotify-sys (ISC) / mio (MIT) /
    libc (MIT OR Apache-2.0) / bitflags (MIT OR Apache-2.0) / log (MIT OR Apache-2.0) /
    notify-types (CC0-1.0) / walkdir (既存)。
    kqueue/fsevent-sysはmacOS向けoptional feature (Linux buildには入らない)。
  - 対応プラットフォーム: Linux/macOS/Windows (backend自動選択)。
  - バージョンpin理由: 9.0.0系は2026-08時点でrc (9.0.0-rc.4) のため
    安定版の8系最新をpin。
- **`gento-core`への新規外部依存なし**: 走査エンジン (`library` module) は
  stdのみ (trait `RepoStore`/`ScanFs` でDB実装とfs実装を注入。計画書§5
  「外部機能はtraitを介して注入」に従う)。

## WP-3 (MVP) で追加・変更した依存 (2026-08-30。MVP画像形式と表示パイプライン)

- **`image` =0.25.10 (default-features=false, features: png, jpeg, gif, bmp, tiff, webp) へfeature拡大**
  - 用途: JPEG/GIF/BMP/TIFF/WebPのデコード (WP-3形式拡張。計画書§11)。
    PNG専用だったfeature構成をMVP 6形式へ拡大。判定はmagic (先頭バイト)
    ベースで `ImageReader::with_format` を明示指定 — 拡張子詐称にも
    magicが正本。`Limits` (寸法20000・alloc 256MiB) は全形式へ適用。
  - 追加される推移依存 (いずれもMIT OR Apache-2.0):
    zune-core / zune-jpeg (JPEGデコード。pure Rust) /
    gif =0.14 + color_quant (GIF) / tiff (TIFF) / image-webp =0.2 (WebP。
    pure Rust。アニメーションWebPのフレーム分割デコード対応)。
    `rayon` は引き続き無効 (サムネイル生成パスは非並列で十分)。
  - AVIF: `avif-native` feature (mp4parse + dav1dシステムライブラリ) は
    別ゲート (本環境にシステムdav1dが存在しないためblocked-avif。
    詳細は docs/handoff/2026-08-30-mvp-wp3.md)。
- **`kamadak-exif` =0.5.5 (gento-image, 通常依存)**
  - 用途: EXIF Orientation読み取り (JPEG APP1 / TIFF / PNG eXIf / WebP EXIF)。
    Orientation反映はdecode後のメモリ上変換のみで、元ファイルは変更しない。
  - 入手元: crates.io (kamadak/exif-rs)。
  - ライセンス: **BSD-2-Clause**。
  - 依存するnative library: なし (pure Rust)。
  - 対応プラットフォーム: 全主要OS。
  - バージョンpin理由: 0.5系最新。コンテナ対応 (JPEG/TIFF) は0.5系で
    安定。PNG/WebPのEXIFチャンクは本repo側で抽出してTIFF生体を渡す
    (crateのコンテナ対応に依存しない)。

## AVIF-1 (WP-3残項目) で追加した依存 (2026-08-31。AVIF有効化)

- **`image` =0.25.10 のfeatureに `avif-native` を追加** (workspace。既定ON)
  - 用途: AVIFのprobe・decode (WP-3で前方実装済みの `ProbedFormat::Avif` 経路の
    有効化。dav1d 1.4.1導入に伴うもの。handoff 2026-08-30-mvp-wp3.md の
    blocked-avif解消)。
  - 追加される推移依存:
    - **`dav1d`** =0.11.1 (rust-av/dav1d-rs。**MIT**) — AV1デコードのsafe wrapper。
    - **`dav1d-sys`** =0.8.3 (同repo。**MIT**) — **システムライブラリ `libdav1d`
      への動的リンク** (`system-deps`経由のpkg-config `dav1d` ≥1.3で発見。
      bindingsは手書き宣言でbindgen不要。システムにdav1dが無い場合は
      git clone + meson/ninjaでdav1d 1.5.0をソースビルドするfallbackがある
      — CI/配布環境では明示的にシステムdav1dを入れる方が決定的。
      検証環境: libdav1d7 / libdav1d-dev 1.4.1-1build1 Ubuntu 24.04)。
    - **`mp4parse`** =0.17.0 (mozilla/mp4parse-rust。**MPL-2.0**) — AVIF (BMFF)
      コンテナのパース (HeifContext経由)。未改変のライブラリ利用のため
      MPL-2.0の義務は発生しない (改変時にファイル単位の公開義務に注意)。
  - 依存するnative library: **libdav1d (システム共用ライブラリ)**。
    - Linux: `libdav1d-dev` (ビルド時) + `libdav1d7` (実行時)。Ubuntu 24.04では
      標準universeリポジトリから入手可。実行バイナリはdav1dに動的リンクする
      ため、配布物 (AppImage等) ではsoの同梱/依存解決が必要 → **WP-17で再検討**。
    - Windows/macOS: dav1dの入手経路 (vcpkg / homebrew) が未検証のため
      **WP-17で実機確認するまで「Linux検証済み」扱い**。featureはプラットフォーム
      非依存にON (条件付きfeatureにしない — cargoのtarget固有依存は複雑化する
      ため、配布検討のタイミングで判断)。
  - 既知の設計上の注意 (image 0.25.10 `AvifDecoder` の実装に由来):
    - デコーダ構築時にdav1dで**本体をフルデコード**する (ヘッダのみの軽量APIが
      無い)。probe (寸法取得) も同経路のため、AVIFのprobeは他形式より高コスト。
      Limits (寸法20000) は構築**後**の `set_limits` で適用され、dav1d内部の
      フレームバッファ割り当て (寸法詐称bitstream) は上限前チェックの対象外。
      詳細は handoff 2026-08-30-mvp-wp3.md のAVIF-1追記。
  - licenses.md: `scripts/gen-licenses.sh` で再生成 (dav1d/dav1d-sys/mp4parse追加)。

## WP-7で追加した依存

- `libc = "0.2"` (gento-tauri, 通常依存)
  - 用途: SIGTERM/SIGINTのgraceful終了 (self-pipe実装: `pipe` / `signal` /
    ハンドラ内 `write(2)` / 監視スレッド `read`)。フェーズ8「終了安全」の実証に必要。
  - ライセンス: MIT OR Apache-2.0。ワークスペースの依存グラフに既存
    (webkit2gtk系経由) のため新規コンパイルコストなし。
  - 代替案: `signal-hook` / `ctrlc` crate。ハンドラ1本のため直接libc呼び出しを
    選択 (依存追加を最小化)。async-signal-safeな操作のみ使用。

## WP-5 (MVP) で追加した依存 (2026-08-31。評価・選別・タグ・キャプション)

- **`unicode-normalization` =0.1.25** (gento-core、通常依存)
  - 用途: タグ正規化名のNIF正規化 (NFC)。`normalize_tag_name` は
    「trim + NFC + ASCII小文字化 + 内部空白の連続を1つに圧縮」の順で適用し、
    `tags.normalized_name` (UNIQUE) の正本とする (MVP実装計画§13「タグ正規化
    規則を固定する」/ data-model §4.1)。
  - 入手元: crates.io (rustunicode/unicode-normalization)。Rust標準の
    Unicode characterization系クレート群の1つで `str::nfc()` イテレータを提供。
  - ライセンス: MIT OR Apache-2.0。再配布義務なし (通常のOSSライセンス表示
    対象。licenses.mdへ自動生成で載る)。
  - native依存: なし (pure Rust。テーブル内蔵)。全プラットフォーム対応。
  - 代替案: 手書きNFC実装はUnicode結合文字表の維持コストが不当に大きいため
    不採用。NFKC (正規化で全角半角を畳む) は仕様上「ASCII小文字化のみ」と
    しているため不使用 — 全角英数字は別タグとして扱う (仕様判断は
    api-contract §29参照)。
  - 注: ルートCargo.tomlは変更せず `crates/gento-core/Cargo.toml` へ直付け
    (workspace.dependenciesは共有crateのみ。単一crate利用のため)。

### WP-6 追加 (PM補記 2026-08-31)

- **`serde_json`** (gento-core、通常依存) — SmartQuery条件JSONの直列化/検証のため。
  - 入手元: crates.io (serde-json)。workspaceに既存 (gento-db等が使用済み) のため
    gento-coreへ直付け。新規外部依存なし。
  - license: MIT OR Apache-2.0。native依存なし。全対応プラットフォーム。
