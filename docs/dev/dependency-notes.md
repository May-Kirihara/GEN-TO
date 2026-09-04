# 依存ライブラリメモ (Phase 2 / WP-2時点)

- 記録日: 2026-08-28 (Phase 1 → WP-2更新 → WP-3更新 2026-08-29 → WP-4更新 2026-08-29 → WP-5更新 2026-08-29 → WP-0確認 2026-08-30 → AVIF-1更新 2026-08-31 → WP-5 (MVP) 更新 2026-08-31 → WP-10更新 2026-09-02 → WP-11更新 2026-09-02)
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
  - WP-18-1 (2026-09-04): **gento-tauriのdev-dependencyとして同一pinを追加**
    (`db.rs`結合テストが未来schema DBを直接構築するため。テスト専用で
    release成果物には入らない。workspace `=0.31.0` pinを共有)。  - 用途: SQLiteアクセス (WAL・foreign_keys・busy_timeout・migration)。
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

## WP-7 (MVP) で追加した依存 (2026-08-31。安全なファイル操作とUndo)

- **`trash` =5.2.6** (gento-platform, 通常依存)
  - 用途: OSゴミ箱への移動 (WP-7。通常削除はゴミ箱が原則)。Linux実装は
    freedesktop.org Trash Specification に従い `$XDG_DATA_HOME/Trash`
    (`~/.local/share/Trash`) の `files/` + `info/*.trashinfo` へ移動する
    (GIO/D-Bus非依存 — ディスプレイ無しヘッドレス環境でも動作する。
    cargo test/E2Eで確認済み)。
  - 入手元: crates.io (ArturKovecs/trash)。
  - ライセンス: **MIT**。
  - 依存するnative library: なし (Linux実装はpure Rust + libc。
    WindowsはPowerShell/COM、macOSはNSWorkspace実装)。
    推移依存: log (既存) / chrono (default feature。Windows日付処理用。
    MIT OR Apache-2.0) / windows-sys等は対象OSのみ。
  - 対応プラットフォーム: Linux/macOS/Windows (WP-17でWindows/macOS実機確認)。
  - バージョンpin理由: 2026-08時点の最新安定版。5系API (`trash::delete`) は
    2020年以来安定。
  - **ゴミ箱移動の失敗時は通常削除へフォールバックしない** (設計方針3。
    エラー表示のみ)。
  - feature: defaultのまま (`coinit_apartmentthreaded` はWindows COM初期化既定、
    `chrono`はWindowsの trashinfo 日付解析に必要)。
- **`gento-platform` 新crate** (workspace member追加): OS差の土台集約
  (ゴミ箱・OSファイルID取得)。gento-coreにのみ依存 (依存方向:
  platform → core。db/image/tauri → platform)。
  - OSファイルID取得 (`os_file_id_of`) はgento-image/src/scan.rsから移動集約。
    gento-imageは再エポートで追随 (既存外部API互換維持)。
  - ゴミ箱復帰 (Undo用): XDG trashinfo を走査して元パスへ戻す実装は
    自前 (trash crateに単一項目復帰APIが無いため。WP-17でWindows対応検討)。

## WP-7 レビュー対応 (FG-WP7) で追加した依存 (2026-08-31)

- **`libc`** (gento-platform、通常依存。`libc = "0.2"`)
  - 用途: `renameat2(2)` への `RENAME_NOREPLACE` 直接syscall (上書き
    TOCTOU排除の原子的no-clobberリネーム)。`SYS_renameat2`/`AT_FDCWD`/
    errno定数のみ使用。crate新規追加ではなく、gento-tauriが既に直付け
    利用中の同一crate・同一バージョン帯 (0.2) をplatformへも直付け。
  - 入手元: crates.io (rust-lang/libc)。
  - ライセンス: MIT OR Apache-2.0。native libraryなし (pure Rust束縛)。
  - 対応プラットフォーム: 全対応 (renameat2呼び出しはLinuxのみ。
    非Linuxはlink+unlinkフォールバック)。
  - `nix`クレートの採用を見送った理由: 必要なのは単一syscallのみで、
    nix (MIT/OR) を追加すると推移依存が増える一方、直接syscallで
    同じ原子性が得られるため (WP指示の「nixまたは直接syscall」より
    依存最小側を選択)。

## WP-8 キーワード検索 (2026-09-01)

- **`gento-search` 新crate** (workspace member追加): 検索AST・parser・
  FTS MATCH/LIKE割り振り (TextPlan)・スマートコレクション保存形式
  (version 2) の読み書き。依存: gento-core / serde / serde_json のみ
  (新外部依存なし。依存方向: search → core、db/tauri → search)。
  - gento-core依存は旧query_json (WP-6最小条件) → AST変換
    (`legacy_to_query`) とタグ正規化の再利用のため。
- **FTS5 trigram**: bundled SQLite (=0.31.0, SQLite 3.45) の組み込み
  機能。追加依存・featureなし (WP-1の `fts5_is_available_at_runtime`
  テストが前提検証済み)。
- 既知の落とし穴 (採用メモ):
  - **FTS5テーブルに対する `PRAGMA integrity_check` は内部で書き込みを
    試みる**ため、READ_ONLY接続・query_only接続では
    「attempt to write a readonly database」で検証が失敗する。
    backup検証 (migrate.rs) は読み書き可能接続で、`db_status`
    (diagnose.rs) のintegrity_checkはwriter接続で実行するよう分離した。
  - trigramは3文字未満トークンを索引化しないため、1〜2文字クエリは
    検索文書テーブルへのLIKEフォールバック (仕様として文書化)。
  - FTS5仮想テーブルのUNINDEXED列 (asset_id) には索引が張れない。
    rowid=asset_idの1:1管理 + rowid経由のDELETE/INSERTで索引更新を
    O(log n) にする (WHERE asset_idでの相関EXISTSは50k行×全走査に
    なる — 計測で顕在化し修正済み)。

## WP-10 永続ジョブ基盤とOpenAI互換AI (2026-09-02)

- **`reqwest` =0.13.4 (features: `json`, `rustls`, `blocking`。default-features=false)** (gento-ai, 通常依存)
  - 用途: OpenAI互換endpoint (`/v1/chat/completions`, `/v1/models`) へのHTTP通信。
    `blocking` featureで同期クライアントを生成し、AIジョブworkerを既存の
    std::threadワーカー規約 (library.rsのハッシュ会計スレッド等) と同じ
    モデルで実装する (tauri async runtime上でのブロッキング回避と、
    cargo test単体テストでの直接実行の両立)。
  - **`default-features=false` の理由**: 既定の `default-tls` はnative-tls
    (Linuxではopensslリンク) を引き込む。`rustls` featureで純Rust TLSに
    限定し、opensslのnative依存を回避 (PM計画§6の指示)。
    rustlsのproviderはaws-lc-rs (`__rustls-aws-lc-rs`経由)。aws-lc-rsは
    C+アセンブリビルド (cc。bindgen不要) — bundled SQLiteと同様の
    「ccでビルドできるものは許容」枠。
  - `http2`/`charset`/`system-proxy`を無効化: ローカルllama.cpp等は
    HTTP/1.1で十分、JSON応答はUTF-8、また環境変数プロキシ
    (`http_proxy`等)によるlocalhost宛て通信の意図しない迂回を避けるため
    直結とする (プロキシ対応が必要になった時点で再検討)。
  - 入手元: crates.io (seanmonstar/reqwest)。
  - ライセンス: **MIT OR Apache-2.0**。
  - 推移依存 (主要): hyper 1 / hyper-util / hyper-rustls / tokio-rustls /
    rustls / aws-lc-rs (ISC + MIT OR Apache-2.0 with OpenSSL例外) /
    serde_json (既存) 等。native library: なし (aws-lc-rsはccでセルフビルド)。
  - 対応プラットフォーム: 全主要OS。
- **`keyring` =4.2.0 (既定features = `v1`)** (gento-ai, 通常依存)
  - 用途: APIキーのOS資格情報ストア保存。LinuxではSecret Service
    (GNOME Keyring等) をzbus (pure Rust D-Bus) 経由で利用、macOSは
    Keychain、WindowsはWindows Credential Manager (feature `v1` の
    既定構成 — プラットフォーム毎のstore選択をcrateに任せる)。
  - 入手元: crates.io (keyring-rs。repositoryは
    open-source-cooperative/keyring-rsへ移管されている形式 —
    licenses.md生成結果に基づき記録)。
  - ライセンス: **MIT OR Apache-2.0**。
  - 推移依存: zbus 5.19.0 (MIT OR Apache-2.0) はtauri経由でCargo.lockに
    既存在存のため新規導入ではない。LinuxのSecret Serviceが利用不能な
    環境 (ヘッドレスCI等) では保存時にエラーとなるため、アプリデータ
    ディレクトリ直下の専用ファイル (0600・平文・警告文同梱) への
    フォールバックを実装 (docs/handoff/2026-09-02-mvp-wp10.md 参照)。
  - 対応プラットフォーム: Linux/macOS/Windows (WP-17で実機確認)。
- **`base64` =0.22.1 (default features)** (gento-ai, 通常依存)
  - 用途: 画像のdata URL化 (`data:image/jpeg;base64,...`)。reqwestが
    既に利用中の同一バージョンをCargo.lockに持つため重複導入なし。
  - 入手元: crates.io (marshallpierce/rust-base64)。
  - ライセンス: **MIT OR Apache-2.0**。native依存なし。
- **`image` (既存 =0.25.10) の利用拡大**: AI送信前の画像縮小
  (最大辺1024px・JPEG再エンコード) で既定featureのcodec構成をそのまま
  使用 (新規featureなし)。デコード制限 (`Limits`) もサムネイル経路と
  同一値を適用。

## WP-9 画像メタデータ (2026-09-02)

- **`flate2` =1.1.10 (gento-image, 通常依存)**
  - 用途: PNG zTXt / iTXt (compressed) チャンクのzlib解凍。
    **Cargo.lockに存在する既存crateの直依存化** (png =0.18.1が
    IDAT解凍で利用中。推移依存のminiz_oxide 0.9.1 = pure Rust。
    新規crateの導入ではない)。
  - 入手元: crates.io (rust-lang/flate2-rs)。
  - ライセンス: MIT OR Apache-2.0。native libraryなし (rust_backend
    既定 = miniz_oxide)。
  - 対応プラットフォーム: 全主要OS。
  - 安全策: 解凍出力にもチャンク上限 (1チャンク4MiB) を適用 —
    圧縮bompに対するメモリ保護 (Read側を`take`で打ち切り)。
- **`serde_json` =1.0.151 (gento-image, 通常依存)**
  - 用途: メタデータparserの出力 (raw_* 列へ保存するJSON文字列) の
    構築。workspaceに既にpin (gento-search等が利用中。新規crateではない)。
  - 入手元・ライセンス: serde同様 (MIT OR Apache-2.0)。
- **kamadak-exif =0.5.5 (既存・WP-3導入) の利用拡大**:
  Orientation専用だった`Reader`を写真メタデータ全般 (撮影日時・カメラ・
  レンズ・GPS) の解析へ拡大利用。コンテナ抽出はcrateの
  `read_from_container` (JPEG/TIFF/PNG/WebP) に一任し、読み取り総量上限
  (64MiB) はrepo側のRead wrapperで強制する。

## WP-11 ruri-v3埋め込み (2026-09-02)

- **`gento-embedding` 新crate** (workspace member追加): ONNX埋め込みの
  実行基盤 (manifest検証・モデル管理・provider・workerプロトコル・DL)。
  独立バイナリ `gento-embedding-worker` (同crateの `[[bin]]` target) は
  tauriコアからspawnされstdioで通信する (設計正本 docs/embedding-runtime.md
  §5)。依存方向: embedding → core。db (build_search_doc_input) / tauri →
  embedding。
- **`ort` =2.0.0-rc.13 (default-features=false, features: `std`, `ndarray`, `download-binaries`, `tls-rustls`)** (gento-embedding, 通常依存)
  - 用途: ONNX RuntimeのRust束縛。ruri-v3 ONNXグラフ
    (inputs=`input_ids`/`attention_mask`, output=`last_hidden_state`,
    opset 17。CONV-1変換・検証済み) のCPU推論実行。
  - **安定版が存在しないRC版である旨のリスク記録 (WP指示)**: pyke-io/ort
    の2.0系は2024年末からRCが長期継続中。rc.13 (2026-07-28) は
    ONNX Runtime 1.28を束縛。API破壊の可能性を残すため、利用箇所を
    gento-embedding::provider (1ファイル) に隔離し、バージョン更新時の
    影響範囲を最小化する。
  - **onnxruntime binary取得方式**: `download-binaries` featureにより
    **build時に** `cdn.pyke.io` (pyke.ioが配布するONNX Runtime公式ソース
    ビルド済みバイナリ) から `x86_64-unknown-linux-gnu.tar.lzma2` をDLし
    (dist.tsv内のsha256で固定・検証)、**静的リンク** (libonnxruntime.a)
    する。生成バイナリはruntimeにonnxruntime共有ライブラリを必要としない。
    リスク: (1) クリーンビルド時にネットワークDLが発生 (CDNダウン時
    ビルド不可 — `ORT_LIB_PATH`等の環境変数で手元ビルド済みlibを指す
    ことで迂回可能)、(2) CDN配布の中断 (404/410応答時、ortは明確な
    エラーメッセージを出す)。
  - **配布license**: ONNX Runtime本体は **MIT**
    (https://github.com/microsoft/onnxruntime/blob/main/LICENSE)。
    静的リンクしても再配布義務はMITの表示保持のみ。ort crate自身は
    **MIT OR Apache-2.0**。`tls-rustls` (DL経路のTLS) はring
    (Cargo.lock既存 0.17.14) をproviderに使用 — opensslリンク回避
    (reqwestのrustls方針と同一)。
  - `default-features=false` の理由: 既定の `tracing` featureを外し
    ログ経路を明示のままにするため。`ndarray` はテンソル構築
    (Array2/i64) のみに使用 (ortのoptional依存と同一version 0.17系を
    gento-embeddingも直付け — feature有効化に必要)。
  - 入手元: crates.io (pyke-io/ort)。
  - 対応プラットフォーム: Linux/macOS/Windows (prebuilt配布あり。
    WP-17でWindows/macOS実機確認)。
- **`tokenizers` =0.23.1 (default-features=false, features: `fancy-regex`)** (gento-embedding, 通常依存)
  - 用途: ruri-v3のtokenizer.json直読み (CONV-1検証済みの正本方式。
    LlamaTokenizerFast系はfrom_slow問題があるため`Tokenizer::from_file`
    を使う — GaOTTT教訓)。Unigramモデル + Metaspace pre-tokenizer +
    ByteFallback decoderはすべてtokenizers crateの**ネイティブ実装**で
    動作。`enable_truncation(max_length=manifest値)` を必ず設定する
    (特殊トークン位置が正しくなる — CONV-1教訓)。
  - **`fancy-regex` feature必須の判明**: tokenizers 0.23.1は
    default-features=falseだと「onigまたはfancy-regexを有効化せよ」の
    compile_errorを出す (src/utils/mod.rs。regex pretokenizer/normalizer
    のruntime解決のため)。onigはC library (oniguruma) リンクのため、
    **純Rust実装の`fancy-regex`を選択**した。fancy-regex (MIT OR
    Apache-2.0) はbit-set/regex-automata等の純Rust依存のみ。ruri-v3の
    tokenizer.json自体はRegex pre-tokenizerを持たないため、実行パスで
    使用されない予備である点も記録する。
  - 入手元: crates.io (huggingface/tokenizers — Rust本体)。
  - ライセンス: **Apache-2.0** (HF tokenizersはApache-2.0単独)。
  - 推移依存: fancy-regex / spm_precompiled (dioma/NFDK依存なし)。
    native libraryなし。
  - 対応プラットフォーム: 全主要OS。
- **`ndarray` 0.17系 (default-features=false)** (gento-embedding, 通常依存)
  - 用途: ortへの入力テンソル構築。ortの `ndarray` featureが要求する
    同一バージョン帯 (ortはoptional依存として0.17を指定) を直付け。
    Cargo.lockに新規追加されるが、実体はort利用に必須の配列型のみ。
  - 入手元: crates.io (rust-ndarray)。ライセンス: MIT OR Apache-2.0。
- **`sha2` =0.10.9 (default features)** (gento-embedding, 通常依存)
  - 用途: モデルパッケージ検証 (manifestのsha256)。**Cargo.lockに既存**
    (0.10.9 = 推移依存として存在。直依存化のみで新規crate導入ではない。
    0.11系もlock内にあるが、安定版は0.10系)。
  - 入手元: crates.io (RustCrypto/hashes)。ライセンス: MIT OR Apache-2.0。
- **`libc` 0.2系 (既存pin帯)** (gento-embedding, 通常依存)
  - 用途: モデルDL前の空き容量確認 (`statvfs(3)`)。gento-tauri/
    gento-platformが利用中の同一crate (0.2.189がlock内) の直付け拡大。
  - ライセンス: MIT OR Apache-2.0。非Linuxでは空き容量確認をskip
    (fail-open) とし、書き込み失敗時に通常のIOエラーで落ちる。
- **`reqwest` (既存 =0.13.4) の利用拡大**: モデル配布repoからのDL
  (進捗・キャンセル・Range再開・プロキシ環境変数尊重)。gento-aiの
  クライアントとは**分離した別Clientインスタンス** (設計§4「プロキシ
  環境の考慮」— gento-ai側はsystem-proxy無効化しているのに対し、
  DL側は`Proxy::system`/環境変数既定を尊重する)。同一crate・同一version
  のため新規依存ではない。

## WP-12 USearch索引・ハイブリッド検索 (2026-09-02)

- **`usearch` =2.26.2 (default features = `numkong`のみ)** (gento-embedding,
  通常依存)
  - 用途: 埋め込みベクトル (ruri-v3・L2正規化済み) のHNSW近傍探索索引。
    cosine metric・f32スカラーで使用 (設計 docs/embedding-runtime.md §6-7)。
  - 入手元: crates.io (unum-cloud/USearch。Rust束縛は同repo `lang/rust`)。
  - **配布license: Apache-2.0** (usearch本体・Rust束縛とも)。静的リンクに
    再配布義務は表示保持のみ。
  - **native依存のビルド要件 (重要)**: crateにC++ヘッダ群 (usearch本体は
    header-only) が同梱され、`build.rs`が **cxx-build でC++17としてその場
    コンパイル**する。加えてSIMDカーネルは `numkong` crate (>=7.5,<8) が
    同様にネイティブソースからコンパイルし静的リンク (`links` metadata経由
    のinclude path連携)。**ビルド時に C++17対応コンパイラ (g++/clang++/MSVC)
    とcmake不要 (cc系のみ)** を要求する。Linux g++ 13.3で確認済み。
    クリーンビルド時間が+1〜2分程度増える。
  - features: `openmp`は**無効のまま** (§7「CPU使用率は控えめ既定」—
    OpenMPリンク (libomp動的リンク) が配布を複雑化するため不使用。
    並列構築はホスト側からインデックスを分割しない本設計では単一
    バックグラウンドスレッドで行う)。`simsimd`はエイリアス (numkongと
    同一) のため不使用。
  - 依存方向: gento-embedding内の新module `vector_index` のみで使用
    (1ファイルに隔離 — version更新影響を最小化するWP-11のortと同じ
    規約)。gento-searchはusearchに依存させない (RRF融合は純Rust)。
  - 対応プラットフォーム: Linux/macOS/Windows (C++17があれば動作。
    WP-17でWindows/macOS実機確認)。
  - 版の保守状況: unum-cloud/USearchは活発開発中 (2.26系。2026-08-31
    リリース)。Rust APIはcxx bridge経由で安定だが、minor更新で
    `IndexOptions`等のフィールドが増える可能性あり — 更新時は
    `vector_index.rs` のみ追従すればよい構造にしてある。

## WP-13 バックアップと復元 (2026-09-02)

- **`serde_json` (既存 =1.0.151) の dev依存→通常依存への昇格** (gento-db)
  - 用途: `.gento`ポータブルバックアップの `backup-info.json` /
    `collections.json` / `metadata.jsonl` の直列化・解析。従来
    dev-dependency (テスト専用) だったものを通常依存へ昇格するのみで、
    workspace単位の版本pin・Cargo.lock実体とも変更なし
    (gento-core等が同じpinを既に使用)。
- **`blake3` (既存 =1.8.7) の直依存化** (gento-db)
  - 用途: バックアップファイル_digest・復元時のon-disk内容ハッシュ
    (`blake3:<hex>` 形式はgento-imageと同一算法・同一表現)。
    gento-image / gento-tauriが利用中の同一crate・同一versionの
    直付け拡大のため新規外部依存ではない。gento-dbからgento-imageへ
    依存を張るとprobe系を巻き込むため、rawハッシュ計算のみ直付けした。
  - 入手元: crates.io (BLAKE3 team)。ライセンス: CC0-1.0 OR Apache-2.0。

## FG-WP13-1 レビュー対応 (2026-09-03)

- **`happy-dom` =20.13.2** (desktopフロント、dev依存)
  - 用途: `RestoreDialog.test.ts` のコンポーネント描画テスト専用のDOM
    環境 (`@vitest-environment happy-dom` docblockでファイル単位のみ有効。
    他テストは従来どおりnode環境)。ビルド成果物・実運用コードには影響
    しない。vitestはsvelteをクライアント版で解決する必要があるため、
    `vite.config.ts` に `process.env.VITEST` 条件でのみ
    `resolve.conditions: ["browser"]` を追加した (dev/build無関係)。
  - 入手元: npm (capricorn86/happy-dom)。ライセンス: MIT。

## FG-WP13-2 レビュー対応 (2026-09-03)

- **`libc` 0.2系 (既存pin帯)** (gento-core、通常依存。`libc = "0.2"`)
  - 用途: 復元実体ハッシュ計算前の**全コンポーネントsymlink不追従オープン**
    (`open_regular_beneath`)。Linuxでは `openat2(2)` +
    `RESOLVE_BENEATH|RESOLVE_NO_SYMLINKS` の単一原子呼び出しを試行し
    (ENOSYS/EINVALは不対応扱いでフォールバック)、全unixでは
    `openat(2)` + `O_DIRECTORY|O_NOFOLLOW` のfd歩行にフォールバック
    (中間コンポーネントのsymlink脱出を構造的に遮断 — WP-13レビュー
    blocker対応)。`open`/`openat`/`syscall`/`fcntl`/フラグ定数のみ使用。
  - 入手元: crates.io (rust-lang/libc)。ライセンス: MIT OR Apache-2.0。
    native libraryなし (pure Rust束縛)。
  - gento-tauri / gento-platform / gento-embeddingが利用中の同一crate
    (0.2.189がlock内) の直付け拡大のため、新規外部依存ではない。
  - 呼び出しは新module `gento-core/src/path_safety.rs` に隔離
    (WP-11 ort / WP-12 usearchと同じ「1ファイル隔離」規約)。
    非unix (Windows) は字句的symlink検査のフォールバックで、構造的
    (race-free) 保証はunix実装のみ (WP-17で再検討)。

## WP-15 設定・診断・プライバシー (2026-09-03)

- **新規外部依存なし**。以下はworkspace既存pinのcrate間付替え (lock変化なし):
  - **`tracing` =0.1.44** (gento-db、通常依存)
    - 用途: 設定値破損時に既定値へフォールバックした事実のwarnログ
      (`settings` module。WP-15受け入れ「設定破損時に安全な既定値で起動」
      の可視化)。それまでgento-dbはログ出力を持たなかったが、破損を黙って
      吸収すると診断 (settingsCorruptCount) と実挙動が繋がらなくなるため
      付替えを判断した。
    - 入手元: crates.io (tokio-rs/tracing)。ライセンス: MIT。
      gento-tauri / gento-image / gento-ai / gento-embeddingが利用中の
      同一crateのため新規外部依存ではない。
  - **`tracing-subscriber` =0.3.23 (env-filter feature付き・既定)**:
    reload handle (`reload::Layer`) はfeature追加なしで利用可能
    (確認済み)。ログring (MakeWriter実装) もstdのみで実装し、
    tracing-appender等の追加は不要。
