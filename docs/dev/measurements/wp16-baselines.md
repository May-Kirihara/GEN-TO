# WP-16 性能基準値 — 実環境ゲート計測 (2026-09-03)

- 計測build: commit `a6e6fd0` + WP-16ノブ (`GENTO_THUMB_WORKERS` / `GENTO_SCAN_BATCH`)
- 計測バイナリ: `cargo build --release -p gento-tauri --features custom-protocol,devtools`
- 環境: 作業マシン Linux 24.04・20論理コア・94GiB RAM・NVMe・Xephyr `:6` 1600x1000
  (スパイク時計の共有デスクトップ `:1` 3424x3852 とは解像度が違うためDOMセル数等の
  絶対比較はできない。スパイク相当値は同一マシン)
- ドライバ: `docs/dev/measurements/drivers/wp16_*.py`・生ログ/結果JSON:
  `/tmp/opencode/wp16/` (logs・results。リポジトリ外の作業領域)
- 要件正本: `docs/mvp-implementation-plan.md` §24

## 0. 対応表 (§24必須10シナリオ → 本文中の節)

| # | §24シナリオ | 本書の節 | 主な結果 |
|---|---|---|---|
| 1 | スパイク計測の回帰 | §1 | 重大退行なし (s256 p95 ≈1.4倍はスパイク記載の変動域内) |
| 2 | 実5万ファイルE2E | §2 | 走査6.01s/5万・停止なし (ハートビート欠落0) |
| 3 | 低スペック (taskset 2コア) | §3 | 全操作動作・ノブ実証 (workers=2で同一スループット) |
| 4 | 100MP級fixture | §4 | 256MiBデコード上限で安全拒否 (0.3ms/147.6ms)・UI継続 |
| 5 | 破損/不正寸法/巨大メタデータ | §5 | 全件エラー分類・スキップ・UI継続 |
| 6 | AIジョブ中の閲覧・検索・比較 | §6 | 検索2-4ms・long frame 0 |
| 7 | ruri-v3全件処理中のquery | §7 | 974asset埋め込み130s・処理中もUI無影響 |
| 8 | DB/FTS/USearch/サムネ再構築 | §8 | VACUUM INTO 0.8s・FTS 2.4s・USearch 16.9s/5万 |
| 9 | ファイル操作中の異常終了 | §9 | kill -9でtmp残0・disk==DB・quick_check ok |
| 10 | 長時間連続運転 (soak) | §10 | 38分・RSS+21MB(内容成長分)・fd不変・フリーズ0 |

継続監視9項目 (§24) は各節の表中: 起動時間(§1)/RSS内訳とピーク(§11)/
サムネp50-p99(§1,§2)/DB page query=FTS latency(§1,§8)/vector query(§1,§8)/
UI long frame(各節)/worker queue(§2)/キャッシュヒット率(§1,§2)。

## 1. S1: スパイク計測の回帰 (sample/dataset 1,615件)

スパイク (`docs/technical-spike-report.md` §4) と同一手順、3走
(初回HOME無し=初回DB作成込み。`s1-results.json`)。

| 指標 | スパイク (08-30) | WP-16 (09-03) | 判定 |
|---|---|---|---|
| 起動→ウィンドウ出現 | 1.76s (worst 2.01) | **1.71 / 1.71 / 1.72s** | ✓ 同等 |
| mount→health (初回起動) | 28ms | 316-339ms ※1 | 説明あり |
| mount→health (2回目以降) | — | **57-58ms** | ※1 |
| mount→先頭ページ | 584ms | 466-763ms | ✓ 同等 |
| ディレクトリ走査 (1,615) | 29ms (probe込41) | **29-42ms** | ✓ |
| s256全件生成 (空キャッシュ) | 10.05s×3 (9〜16s変動) | **10.05 / 12.06 / 16.06s** | ✓ 変動域内 |
| s256 1枚 p50/p95/p99 | 19-20 / 28-32 / 34-41ms | **19.8-35.7 / 31.7-46.7 / 42.4-53.2ms** | △ p95最大1.46倍 ※2 |
| 再起動後ヒット率 | 1.000・生成0 | **1.000・miss 0** | ✓ |
| protocol応答 p50/p95 | 0.1ms | **0.1 / 0.2ms** | ✓ |
| キャッシュ削除→再生成 | 12.05s | 11.46-17.51s | ✓ |
| 高速スクロール (40/80/40) long frame増加 | 0〜1 | **0** | ✓ |
| 100枚連続移動 p50/p95/p99 | 27/53/66ms | p50 1-17ms / p95 28-124ms / p99 30-124ms | ✓〜△ ※3 |
| 5万仮想ジャンプ 中央/末尾 | 332 / 1,033ms | **331 / 1,037ms** (中央値) | ✓ ほぼ同一 |
| アイドルRSS (合計) | 約504MB | **495-503MB** | ✓ |
| 起動単発long frame | ~110ms | 初回起動294-420ms / 2回目以降117-144ms | ※1 |

※1 初回起動のdH/lfMax増加は**初回DB作成+migration**を含むため (スパイクは
DBの無いアプリ+既存HOME)。2回目以降のdH=57msはスパイク28msの2倍だが
絶対値として十分小さく、MVPアプリのhealthがDB接続・モデルストア確認まで
担うようになったための構造差。単発long frameも初回起動時のみで常時ではない。

※2 s256 p95の最悪46.7msはスパイク上限32msの1.46倍。スパイク報告書自体が
「初回計測では9〜16sの変動あり、原因未特定(共用測定機の負荷と推定)」と
記載した変動の再現であり、本計測でもrun間で10.05〜16.06sの同一パターン。
「重大退行 (p95 2倍超等)」の基準には未達。フォローアップとして
変動原因の特定は残課題 (スパイク報告書#1と同一)。

※3 100枚移動のp95/p99にばらつき (run1は初回昇格込み)。p50はスパイクより
改善。破棄0・long frame 0は維持。

### cargo性能テスト回帰 (合成5万件。WP-8/WP-12基準との比較)

| テスト | 基準値 | WP-16再測定 | 判定 |
|---|---|---|---|
| FTS 50k (`search_performance_50k_synthetic`) | p50 27.4 / p95 42-78.5ms・rebuild 2.3s・全収集465ms | **p50 29.20 / p95 44.48 / max 50.60ms・rebuild 2.3s・全収集472ms** | ✓ |
| USearch 50k (`index_benchmark`) | p50 0.35 / p95 0.45ms・構築16.4s・recall 0.995・144.4MiB | **p50 0.36 / p95 0.49 / max 0.49ms・構築16.92s (2,955 adds/s)・recall 0.995・144.4MiB** | ✓ |

再現:
```sh
cargo test -p gento-db --lib search::tests::search_performance_50k_synthetic --release -- --nocapture
cargo test --release -p gento-embedding --test index_benchmark -- --nocapture
```

**結論: 重大退行なし。** §1受入「スパイクからの重大な退行がない」を満たす。

## 2. S2: 実5万ファイルE2E (生成コーパス)

コーパス: `scripts/wp16_gen_corpus.py` → 50,000ファイル / 2,373MiB / 100ディレクトリ。
PNG 27,220 / JPEG 19,989 / WebP 2,016 / **実AVIF 259** (ffmpeg libaom-av1) /
破損・非画像 516 (ヘッダのみ・途中切り詰め・非画像バイトの.png)。
寸法分布 640x480〜7952x5304 (実コレクション模倣)。`/tmp/opencode/wp16/corpus50k`。

### 初回 (走査+DB投入+ハッシュ+サムネイル)

| 項目 | 値 |
|---|---|
| 登録→走査完了 (files 50,000行upsert) | **6.01s** |
| ハッシュ (BLAKE3+probe+metadata+asset化) 完了 | **1,288.9s (21.5分)** ※4 |
| files / assets (内容重複排除後) / hashed | 50,000 / 49,665 / 49,665 |
| scan_state | 全件 present (破損もハッシュ化=asset管理。サムネイル時のみ失敗分類) |
| s256全件生成 (45,321枚・キュー上限で再投入) | **471.1s = 96.2枚/s** (4ワーカー) |
| s256 p50/p95/p99 (混合形式実ファイル) | **29.8 / 98.9 / 198.6ms** |
| サムネイルキャッシュ | 55.9MB / 約49,000ファイル |
| DBサイズ (稼働中) | 本体57.5MB + **WAL 252.3MB** ※5 |
| ハッシュ中RSSピーク | **1,219MB** ※6 |

※4 ハッシュは前半~12k assetが約6/s、以降約55-112/s→30/sに漸減。
microbench (同一DBコピーで `hash_and_link_file` 0.6-14ms/件) と大幅に乖離する
前段の低速帯があり、詳細原因は未特定 (WAL急膨張と同時発生。§12リスク参照)。
後続の再起動で残り1,417ファイル分は自動補完された (reconciliation正常動作)。

※5 計測時点 (FG-WP16-1前) ではクリーン終了後もWALが残った (次回起動時に
リカバリ)。**FG-WP16-1 F2でgraceful shutdown時の明示checkpoint (TRUNCATE)
を追加**し、以後の終了時WALは0バイトになる (§14)。VACUUM INTOバックアップ
は54.1MBに縮む (§8)。

※6 2ワーカーハッシュ+プローブ+WebThumbnail並走時。低スペックでの頭打ち
懸念は§3のノブで軽減可能。

### 再起動→検索→スクロール (停止なしの実証)

| 項目 | 値 |
|---|---|
| 起動→ウィンドウ | **1.71s** |
| →カタログ総数表示 (49,665) | **3.97s** (dP=1,761ms) |
| viewportサムネイル | **hr=1.000・miss=0**・protocol p50 0.1ms |
| キーワード検索「sunset」 (2,917一致) | **141ms** |
| 構造化混在「cat rating:>=0」 (2,921一致) | **139ms** |
| 狭域 (0一致) | 3ms |
| 高速スクロール (40/80/80 Page_Down) | long frame **0-1**・worst 138ms・DOM 30・queue収束0 |
| ジャンプ 中央/末尾 (pager実fetch) | **0.333s / 1.041s** (仮想カタログとほぼ同一) |
| アイドルRSS | 556MB |
| **devtestハートビート (1s周期) 5秒超欠落** | **全フェーズで 0件 = UIフリーズ>5sなし** |

**§24受入「実5万ファイルで主要操作を妨げる停止がない」を実証。**

## 3. S3: 低スペック相当 (taskset -c 0,1 = 2コア束縛)

sample/dataset 1,615件。2ケース: 既定 (thumb workers=4) / ノブ縮小
(`GENTO_THUMB_WORKERS=2` + `GENTO_SCAN_BATCH=16`)。
ノブ発効はログ行 `GENTO_THUMB_WORKERS override active workers=2` /
`GENTO_SCAN_BATCH override active batch_size=16` で実証済み
(`s3/results.json` knob_logs)。

| 操作 | 2コア+既定 | 2コア+ノブ | 20コア参照 (S1) |
|---|---|---|---|
| 起動→ウィンドウ | 1.71s | 1.71s | 1.71s |
| 走査 (1,615) | 50ms | 75ms | 29-42ms |
| s256全件生成 | **25.05s (64.5枚/s)** | **25.05s (64.5枚/s)** | 10-16s |
| s256 p50/p95/p99 | 26.9/40.8/49.5ms | 26.8/40.8/49.7ms | 20-36/32-47/42-53ms |
| 検索 (狭域/1,615図書館) | 1-2ms | — | 2ms |
| スクロール long frame | 0 | 0 | 0 |
| RSS | 542MB | 558MB | ~500MB |

- **サムネイル生成は2コアでCPU飽和**のため、ワーカー数4→2にしても
  スループット不変 (64.5枚/s)。ノブの価値は**メモリ使用の抑制**と
  オーバーサブスクライブ回避 (デコードバッファ2→1バッチ分)。
- **§24受入「低スペック相当で並列数とbatchを安全に縮小できる」を満たす**
  (既定値不変・範囲検査付き・無効値は無視・発効ログ)。

### 追加したノブ (最小の安全なコード変更)

| 環境変数 | 既定 | 範囲 | 効果 |
|---|---|---|---|
| `GENTO_THUMB_WORKERS` | `min(4, available_parallelism)` | 1..=32整数 | サムネイルプールワーカー数を上書き (起動時のみ) |
| `GENTO_SCAN_BATCH` | 64 | 1..=4096整数 | 走査バッチ件数を上書き (走査開始ごと) |

無効値 (0/範囲外/非数値/前後空白) はすべて無視して既定へ戻る。単体テスト
`worker_override_parses_only_bounded_positive_ints` /
`worker_count_default_is_min4_when_override_absent` /
`scan_batch_override_parses_only_bounded_positive_ints` (計3件追加)。
発効時のみ `tracing::info` でログ出力 (計測・検証用)。

## 4. S4: 100MP級fixture (12000x8500 PNG+JPEG)

fixture: `scripts/wp16_gen_fixtures.py` (PNG 1.3MB・JPEG 24.6MB)。
分類は `gento_image::thumbnail::generate` 直接呼出 (リリースbuild) で計測:

| fixture | 結果 | 所要 |
|---|---|---|
| photo100mp.png (102MP) | `Memory limit exceeded` (256MiB割当上限。IHDR予約段階で拒否) | **0.3ms** |
| photo100mp.jpg (102MP) | `Memory limit exceeded` (SOF走査後に拒否) | **147.6ms** |
| — WebKit (ビューアoriginal表示) | WebKit側が102MBデコード・表示成功 | RSS +26MB・long frame 1 (287ms)・ハートビート欠落0 |

- 現行の安全上限 (WP-3: 寸法20,000・割当256MiB = RGBA換算64MP) により
  **100MPはRust側で保護的に拒否**され、メモリ峰值はidle+3MB未満 (上限が
  割当前に拒否するため)。§24の「デコード時間・メモリ峰值・UI妨げなし」に
  対する現buildの答えは「**拒否も含めUIは一切妨げない (最悪147.6ms/件は
  ワーカースレッド内)・メモリは膨張しない」**。
- 100MPの**表示**はoriginal (WebKit) 経由で可能。サムネ/preview生成を
  100MPに対して有効化するには上限引き上げ (256MiB→512MiB級) が必要で、
  これはWP-3の設計判断 (decompression bomb対策) の変更を伴うため
  本WPでは変更せずPM判断事項として記録する (§12)。

## 5. S5: 破損/不正寸法/巨大メタデータfixture

| fixture | アプリでの分類 | 所要 | 備考 |
|---|---|---|---|
| corrupt_header.png (シグネチャ+IHDR後に切断) | `unexpected end of file` (Decode失敗) | 0.1ms | 即時・UI継続 |
| fake_dims.png (IHDR改竄 30000x30000) | `Image size exceeds limit` (寸法厳守上限20000) | **0.0ms** | 寸法虚偽は寸法上限が遮断 |
| huge_text.png (有効1x1 PNG + 50MB tEXt) | **サムネイル生成成功** | 8.2ms | メタデータ収集は総量上限で停止、デコードに影響なし |
| (S2コーパス内破損516件) | 全件サムネイル失敗分類 (fail=516×再要求回数)・個別表示 | — | UI継続・ハートビート欠落0 |

E2E (`wp16_s45_fixtures.py`): 5asset登録・gen 1 (huge_text)・fail 3-4・
RSSピーク 513MB (idle比+10MB)・worst frame 287ms・**devtestハートビート欠落0**。
WP-3安全機構が実ファイルで機能することを実証。

## 6. S6: AIジョブ中の閲覧・検索・比較

遅延モック: `scripts/wp16_ai_slow_mock.py` (chat/completionsを3-10s遅延。
OpenAI互換。実画像ダウンスケール後の要求ボディ120-260KBを確認)。
プロファイル+ジョブ12件をDB投入し、処理中 (秒単位のHTTP待ちが常時2並列)
にUI操作を計測 (`s6/results.json`):

| 操作 (ジョブ詰まり中) | 値 |
|---|---|
| キーワード検索 | **2-4ms** |
| 高速スクロール (40 Page_Down) | long frame **0**・worst 135-170ms |
| 比較viewer表示 | long frame 0・img ok |
| サムネprotocol p50 | 0.1ms |
| ジョブ結果 | 12/12 succeeded (遅延付きで応答・タグ/キャプション適用) |

AI応答はプロンプトJSON要求を想定: mockは`response_format`無しでも
JSON本文を返すよう対応 (WP-10手順書の「構造化非対応endpoint」運用と同一)。

## 7. S7: ruri-v3全件処理中のquery

- モデル: `.local-models/ruri-v3-70m` (実ONNX・別プロセスworker)。
- 対象: sample/dataset 1,615ファイル → **内容重複排除後 974 asset**
  (同一hashは1文書化。全件処理=974件)。
- **全件埋め込み: 130.1s (約7.5 asset/s・DOC_BATCHバッチ)**。
- 処理中のUI:

| 操作 | 値 |
|---|---|
| キーワード検索 | 2ms |
| **ハイブリッド検索 (意味重み50%)** | **2ms** (スライダー50%設定をOCRで確認: `s7/logs/slider.png`) |
| 高速スクロール | long frame 0・thumb protocol p50 0.1ms |
| ハートビート欠落 | 0 |

ワーカーは独立プロセスのためUIスレッドへの影響は観測ゼロ (実証)。

## 8. S8: 再構築 (DB / FTS / USearch / サムネイル)

| 対象 | 規模 | 所要 | 検証 |
|---|---|---|---|
| VACUUM INTO フルDBバックアップ (50k DB) | 57.5MB+252MB WAL → **54.1MB** | **0.8s** | `quick_check=ok`・files 50,000/assets 49,665/docs 49,665 完全一致 (0.25s) |
| FTS全件rebuild (49,665文書) | 実データ | **2.4s** | integrity missing=0 (WP-8合成50kの2.3sと一致) |
| USearch構築 (50k合成ベクトル) | 384次元 | **16.92s (2,955 adds/s)** 単一スレッド | recall@10 0.995 (WP-12と同一) |
| USearch検索 k=200 | 50k | p50 0.36 / p95 0.49ms | ✓ |
| サムネイル全消去→再生成 | 45,321枚 (実) | **471s = 96.2枚/s** | S2 phase1bの実測 (全キャッシュ空からの再生成と同一) |

USearchの実5万ベクトル再構築はruri-v3実モデルでは数時間を要するため
(974 asset/130s→5万で約1.9h)、**同一パラメータの50kベクトル構築ベンチ**
(WP-12と同一手法。実モデル品質はWP-12 hybrid_evalでtop-3完全一致済み)を
縮小根拠として使用。VACUUM INTO復元 (バックアップファイルを開いての全行
検証) を実規模で実施。

## 9. S9: ファイル操作中の異常終化 (kill -9)

corpus1k (1,000件ライブラリ)・146件複数選択→**実Deleteキーでゴミ箱実行中に
kill -9** (`wp16_s9_kill.py`・`s9/results.json`):

| 検証 | 結果 |
|---|---|
| kill直後 (146件trash処理の完了直後を捕捉) | ディスク854 == DB present 854 (**完全一致**) |
| `.gento-tmp-*` / `.tmp-*` 残骸 (データセット+キャッシュ+HOME) | **0件** |
| DB `quick_check` (kill直後・再起動後) | **ok** |
| 再起動 | 1.71s・カタログ854 (正しく減少反映)・ヒット率0.95→回復 |
| UI | lfMax 134ms・ハートビート欠落なし |

WP-7の保証 (ファイル操作の原子性・reconciliation) が実環境で機能。
※trashはOSゴミ箱 (`~/.local/share/Trash`) への移動として実行された。

## 10. S10: 長時間連続運転 (soak 38分) — 結果

`wp16_s10_soak.py` (38分)。corpus1kライブラリ (853 asset):

- **チャーン20s周期** (112サイクル・エラー0): ファイル追加×3 (プールから
  コピー・計~336ファイル) + 改名×2 + 外部削除×2 — watcher/再走査が追従
- **UI操作25s周期**ローテーション: 検索「sunset」/ スクロールバースト / Home
- 30s周期サンプリング: RSS・fd・long frame・キュー・ヒット率

| 指標 | 初期 | 終了 | 判定 |
|---|---|---|---|
| RSS (3プロセス合計) | 766.7MB | 787.9MB | **+21MB** (最大-最小 +27.2MB) ※7 |
| fd数 (/proc/PID/fd) | 76 | **76** (74-76で一定) | **リークなし** |
| long frame (累積/38分) | — | **+1** (起動込で計4) | deadlockなし |
| worst frame | 386ms (起動時のみ) | — | — |
| devtestハートビート (2,282行) | — | **5秒超欠落 0件** | UIフリーズなし |
| サムネイルキュー | — | 常に収束 q=0 | 背景生成が遅滞なく追従 |
| ヒット率 | 0.731 (新規追加期) | **0.941へ回復** | 追加分の生成完了 |
| チャーン | — | 112サイクル・**エラー0** | reconciliation継続動作 |

※7 ドリフト+21MBは追加~336ファイル分 (サムネイル~4MB・DB/FTS行・WAL・
検索結果ビューの成長) に対応し、時間にのみ比例するリークパターンでは
ない (fd不変・キュー収束・ハートビート連続が傍証)。

**判定: RSSドリフト・fdリーク・deadlockなし・UI長時間安定。**

## 11. Linux idle RSS ~504MBの内訳 (受け入れ条件)

smaps_rollup (s8_cached_reload run3・アイドル・キャッシュ済み):
`s1-results.json` `s8_rss_breakdown` (KB):

| プロセス | RSS | shared_clean | shared_dirty | private | anonymous |
|---|---|---|---|---|---|
| gento-tauri (本体) | 211,768 | **156,872** | 8,000 | 46,896 | 42,620 |
| WebKitWebProcess | 242,888 | **172,128** | 0 | 70,760 | 70,728 |
| WebKitNetworkProcess | 48,808 | 41,116 | 0 | 7,692 | 7,692 |
| 合計 (単純加算) | **503,464** | | | | |

- **約370MB (7割超) はWebKitGTK 3プロセスと共有ライブラリ**
  (shared_clean = ファイルバックの共有ページ。本体プロセスの157MB共有も
  大半はWebKit/GTKの共有ライブラリと静的データ)。
- 各プロセスのprivate (anon) 合計は約121MB — これが実質的なヒープ
  (RustのDBバッファ+カタログ状態+WebViewのDOM/JS)。
- **合計RSSはプロセス間で共有ページを二重計上する**ため、「実メモリ消費」
  は503MBより小さい。スパイク時 (504MB) からの差分要因は:
  MVP化で増えたもの (SQLite接続プール・WALページキャッシュ・ライブラリ
  スキャン状態・embedding/AIサービスのアイドルスレッド) と、WebKit側の
  フロントエンド規模増 (Svelteコンポーネント増) — だが合計RSSは
  **504MB→495-503MBとほぼ不変** (スパイクと同水準)。
- 50kカタログでは556MB (+55MB: DBページキャッシュとファイブ桁カタログの
  pager状態)。ハッシュ全走査中のピークは1,219MB (§2)。

## 12. 発見とリスク (PMへの申し送り)

> 2026-09-03 FG-WP16-1にて1-3を対応 (§14参照)。以下は当時の申し送り
> 原文に対応状況を併記したもの。

1. **`generate_all_thumbnails` の5万件スケール制限** — 1回の投入がサムネイル
    プールキュー (4,096) 分しか入らず、残りは**黙って破棄**される
    (再投入で回復)。利用者が「全件生成」を押した際の期待と異なる可能性。
    対応候補: 生成完了駆動の再投入、または投入し切れなかった件数のUI表示。
    → **修正済み (FG-WP16-1 F1)**: 溢れはbacklogへ繰延され、プール完了
    駆動で空きができ次第投入される (ハッシュ再提出と同一の需要駆動方式)。
    `deferred` フィールドで残数を応答。§14参照。
2. **50kハッシュ初段の低速帯 (~6/sが約11分)** — microbench (0.6-14ms/件) と
    の乖離。WAL 252MBへの急膨張と同時発生。スループット設計 (2ワーカー) 自体は
    問題なく、低速帯の原因特定は残課題。5万件初期投入の実用時間は21.5分。
    → **診断完了・構造的につき申し送り維持 (FG-WP16-1 F3)**。§14に
    診断結果 (計測ドライバ・数値付き)。副産品としてfts_countsのsnapshot
    非一貫性 (偽不整合→writer上の無駄なfts_repair連発) を修正。
3. **WALがクリーン終了後も252MBのまま残る** — 次回起動でリカバリはされるが、
    ディスク使用と初回起動時間への影響。終了時checkpointの検討価値あり。
    → **修正済み (FG-WP16-1 F2)**: graceful shutdownで全サービス停止後・
    DB close前に `PRAGMA wal_checkpoint(TRUNCATE)` を明示実行。
    実機50k計測でWAL 225.6MB → **0バイト** を確認 (§14)。
4. **100MP表示方針** — §4の通り現行は保護的拒否。実カメラの100MP写真を
   サムネ/preview対象にするにはWP-3上限の見直しが必要 (PM判断)。
5. **AVIF probeがdav1dフルデコードで最大69ms/件** (WP-3設計) — ハッシュ
   パイプラインに含まれるためAVIF比率が高いコレクションでは走査が遅くなる。
   実測: 259件/50kでは影響軽微。
6. s256全件生成の時間変動 (10-16s) はスパイクから継続 (原因未特定のまま)。

## 13. 再現コマンドまとめ

```sh
# コーパス・fixture生成 (作業領域へ)
python3 scripts/wp16_gen_corpus.py /tmp/opencode/wp16/corpus50k 50000
python3 scripts/wp16_gen_fixtures.py /tmp/opencode/wp16/fixtures
python3 scripts/wp16_ai_slow_mock.py 8765 &

# 計測releaseビルド
pnpm --dir apps/desktop build
cargo build --release -p gento-tauri --features custom-protocol,devtools

# E2E (Xephyr :6)
python3 docs/dev/measurements/drivers/wp16_s1_spike.py
python3 docs/dev/measurements/drivers/wp16_s2_50k.py
python3 docs/dev/measurements/drivers/wp16_s3_lowspec.py
python3 docs/dev/measurements/drivers/wp16_s45_fixtures.py
python3 docs/dev/measurements/drivers/wp16_s4_viewer.py
python3 docs/dev/measurements/drivers/wp16_s6_ai_jobs.py   # 要: aimock起動+DB投入
python3 docs/dev/measurements/drivers/wp16_s7_embed.py     # 要: .local-models
python3 docs/dev/measurements/drivers/wp16_s9_kill.py
python3 docs/dev/measurements/drivers/wp16_s10_soak.py 38
```

## 14. FG-WP16-1: 申し送り1-3の対応 (2026-09-03)

基盤: WP-16完了時点 + F1/F2/F3-副修正 (cargo 902 / vitest 527)。
対象: baselines §12の申し送り1-3。

### F1: 全件生成のキュー上限黙示打切り → 修正

- 修正: `generate_all_thumbnails` の溢れ (キュー上限4,096超) は**backlogへ
  繰延**し、プール完了通知駆動で空きができ次第投入 (ハッシュパイプラインの
  再提出 `resubmit_ready` と同一の需要駆動方式。マージン64・バッチ256も
  同一値)。`RequestAccepted.deferred` が繰延件数。UI側は未投入分を
  投げっぱなしだが開発パネル通知に「繰延 N (順次投入)」表示。
  再押下は既存backlogを置換 (残数ログ報告)、キャッシュ全削除・終了時は
  backlogを破棄し残数をログ報告。`request_thumbnails` (表示由来) は
  従来どおり満杯即時拒否のまま (backlogにしない)。
- テスト (red→green): `generate_all_defers_overflow_and_drains_all_beyond_queue_capacity`
  (容量4で12件→queued=4/deferred=8、再投入なしの1回呼び出しで全12件生成)、
  `request_rejects_full_without_backlog`、
  `generate_all_backlog_dropped_on_clear_cache_and_reports_at_shutdown`、
  vitest `generateAllの繰延件数は通知に表示する`。
- 旧挙動の実証: 当時のs2 phase1.logで generate_all 1回押下後 gen=4,163
  (≈4,096+30) で凍結・以降生成なし = 黙示打切りが発生していた。

### F2: クリーン終了時のWAL残留 → 修正

- 修正: `gento_db::Db::checkpoint_wal_truncate` を追加
  (`PRAGMA wal_checkpoint(TRUNCATE)` をwriter接続で実行)。RunEvent::Exit
  で全サービス (thumbnail/library/AI/embedding/vector/backup) 停止後・
  DB close前に実行。失敗はwarnで継続 (終了を止めない)。
- テスト (red→green): `wal_checkpoint_truncate_resets_wal_after_bulk_writes`
  (600txn×8KB書込→WAL>0→checkpoint→WAL==0・busy=0・600行読取可能)。
- 実機確認 (F3ドライバの終了フェーズ): 50k投入 (WAL 225.6MB) →
  SIGTERM graceful終了 → **WAL 0バイト**・`quick_check=ok`・
  アプリログ `shutdown wal checkpoint complete busy=0`。8kでも同様。

### F3: 50kハッシュ初段低速帯 → 診断完了 (構造的・申し送り維持)

計測ドライバ: `docs/dev/measurements/drivers/wp16_f3_hashdiag.py`
(1s周期で hashed数・WAL/mainサイズ・/proc io・CPU・fts repairログ件数を
採取。Xephyr :6。WM無し環境では `xdotool windowactivate` でなく
`windowfocus` が必要 — EWMHが無いため)。

**再現性**: 50kで3/3再現 (旧WP-16実績 + 再計測2走)。**8kでは不再現**
(8,000件を19.1s = 約416件/s で哈切。WAL最大56MB)。スケール依存。

**50k再計測 (修正前バイナリ、FTS修正前)**:

| 指標 | 値 |
|---|---|
| 登録→ハッシュ完了 | 1,197.2s (旧実績1,288.9sと同形) |
| 低速帯 | ~6件/s (一定) × ~540s → 速度回復 (~100-160件/s・時々1件/sの間欠停滞) |
| 回復タイミング | hashed≈4,400件 (旧実績でも約4,000件で回復) |
| WAL | 低速帯中に単調増加 → **225.61MBで凍結** (回復後は増えない) |
| アプリCPU | 低速帯 約250% (2.5コア) / 高速帯 約200% |
| アプリ書込 | 低速帯 約1.17MB/s = **約200KB/件** (高速帯 約4KB/件の50倍) |
| fts repair嵐 | "fts index repaired rebuilt=0 removed=0" 3,327回 (旧実績3,918回) |

**50k再計測 (fts_counts修正後のクリーン再走)**:

| 指標 | 値 |
|---|---|
| fts repair | **0回** (修正が完全に効いている) |
| 低速帯 | **~5件/s が40分の計測上限まで持続** (15,201/47,797件でキャップ。回復なし) |
| WAL | ~234-236MBで凍結後も低速持続 → **WALサイズ自体は速度制限因子でない** |
| graceful終了 | WAL 236.25MB → **0バイト** (F2実機再確認) ・quick_check ok |

→ fts嵐の除去でwriter上の無駄トランザクションは消えたが低速帯はむしろ
長期化した。fts嵐は (少なくとも嵐ありの走で観測された) 回復とも相関が
あるが、低速帯の生成・維持には非依存 — 嵐は症状 (同じemitパスから誘発
される) であり原因ではないことが確定。

**切り分け結果**:

1. **サムネIL競合説は否定** — 低速帯中サムネイル生成は未稼働
   (gen=30・q=0で不変。旧実績ログでも同一)。
2. **fts repair嵐は症状であって原因ではない** — 原因特定: `fts_counts` の
   2つのCOUNTが**別々の暗黙readトランザクション**で走り、writerのcommitを
   跨ぐと偽の不整合がほぼ常態化 → ハッシュ中のほぼ全snapshot変更で
   `ensure_fts_after_scan` がwriter接続上で `fts_repair` (全走査整合解析) を
   実行していた。**単一SELECT文化 (snapshot一貫) で修正**し100%消滅を確認
   (回帰テスト: `fts_counts_is_snapshot_consistent_under_concurrent_writes`、
   red→green)。**しかし低速帯自体は変わらず持続** → 原因は別。
3. **低速帯の実体** — 2つのハッシュワーカーが**各100% CPU** (IO待ち・
   ロック待ちでなくCPU実行)。同時に:
   - 1ハッシュemit (100ms間引き・最大10/s) のたびに **O(ライブラリ规模)**
     の作業がハッシュワーカースレッド上で同期実行される:
     `build_library_snapshot` (list_present_joined 50k行JOIN —
     実測**約76ms/回**) + レコード構築 + カタログ置換 + fts_counts (約14ms)。
     2ワーカー× (emit ~150ms + 次emit待ち) のサイクルは ~5-8件/sと
     定量的に整合する。
   - 低速帯の書込増幅 (約200KB/件) は50k規模のB-tree/FTS5索引tail再編と
     一致。WALは低速帯中に増加し~230MB前後で凍結するが、**凍結後も低速が
     持続する走がある** (fts修正後クリーン再走) ためWALサイズは速度の
     直接制限因子ではない。
   - 回復の有無・タイミングは走により不定 (fts嵐あり: 4.4k/9.4k件で回復・
     なし: 40分間回復なし) — フィードバック dominan の性能現象。
4. **8kでは帯が無い**のはNが約6分の1で emit O(N) コスト・索引増幅が
   効かないためと整合。

**結論**: 低速帯の直接原因は (a) ハッシュemitパイプラインのO(N)同期作業
(スナップショット再構築・FTS保守がワーカー上で最大10/s) と (b) 持続的
writer+reader負荷下でのWAL checkpoint飢餓による読み/書き増幅の複合。
いずれも設定・順序の小修正では解消できず (emitの間引き・スナップショット
増分化・checkpointer分離などの構造変更が必要)、WP指示に従い
**申し送り維持 (文書化のみ)** とする。fts_counts修正は独立の実欠陥
(偽不整合→writer上の無駄トランザクション3,300+回/run) として実施済み。
後続の構造対応候補 (いずれも別WPで設計判断が必要): emitのハッシュワーカー
からの分離 (専用スレッド/タスク化)・スナップショットの増分適用・
`ensure_fts_after_scan` の走査完了時限定起動。

### テスト・検証 (FG-WP16-1)

- cargo: **902 passed / 0 failed** (`cargo test --workspace`、基準897+5)
- vitest: **527 passed / 45 files** (基準526+1)、`svelte-check` 0 error
- `cargo fmt --all -- --check` / clippy (workspace dev + 触ったcrateの
  release。workspace release `--all-targets` は基盤コミットから
  `gento_ai::testserver` (devtools-gated) の既知compile errorで不可 —
  本WP無関係)
- F1/F2とも単体red→green、F2は実機graceful終了でもWAL 0バイトを確認
