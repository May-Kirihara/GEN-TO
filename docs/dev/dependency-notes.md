# 依存ライブラリメモ (Phase 2 / WP-2時点)

- 記録日: 2026-08-28 (Phase 1 → WP-2更新 → WP-3更新 2026-08-29 → WP-4更新 2026-08-29 → WP-5更新 2026-08-29 → WP-0確認 2026-08-30)
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
| image | =0.25.10 (default-features=false, features: png) | サムネイル用デコード・リサイズ (WP-3)。PNGのみ有効化し攻撃面を最小化。`Limits` (max_alloc 256MiB・寸法20000) でdecompression bomb対策。`DynamicImage::thumbnail_exact` で拡大なしリサイズ | MIT OR Apache-2.0 | gento-image, gento-tauri (dev: テストPNG生成) |
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

## WP-0での依存変更 (2026-08-30)

- **追加なし (Rust/TS両方)**。devtools feature隔離・AppDirs・panic hook・
  APIバージョンはいずれも既存依存 (dirs / tracing / serde) の範囲で実装した。
  ライセンス一覧の生成手段を `scripts/gen-licenses.sh` として追加
  (成果物: [licenses.md](licenses.md))。

## WP-7で追加した依存

- `libc = "0.2"` (gento-tauri, 通常依存)
  - 用途: SIGTERM/SIGINTのgraceful終了 (self-pipe実装: `pipe` / `signal` /
    ハンドラ内 `write(2)` / 監視スレッド `read`)。フェーズ8「終了安全」の実証に必要。
  - ライセンス: MIT OR Apache-2.0。ワークスペースの依存グラフに既存
    (webkit2gtk系経由) のため新規コンパイルコストなし。
  - 代替案: `signal-hook` / `ctrlc` crate。ハンドラ1本のため直接libc呼び出しを
    選択 (依存追加を最小化)。async-signal-safeな操作のみ使用。
