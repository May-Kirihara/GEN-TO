# FG-1 最終ゲート修正 引き継ぎ (2026-08-30)

## 目的

WP-1〜7完了後の最終ゲートで出た Codex最終レビュー (reject) とQAレビュー
(pass with concerns) の指摘 (F1〜F10) を全て修正した。

## 変更内容 (意図ベース)

### コード修正 + テスト (F1〜F5)

1. **F1 プロトコルTOCTOU閉鎖** (`src-tauri/src/protocol.rs`):
   `ServeSpec` に検証時statの `(dev, ino)` と正規キャッシュルートを持たせ、
   `read_file_checked` を (1) 最終コンポーネントを `O_NOFOLLOW` でopen、
   (2) open済みfdのfstatで正規ファイル・上限・検証時inode一致を確認、
   (3) `/proc/self/fd/{fd}` のreadlink実パスが正規キャッシュルート配下である
   ことを再確認、に強化。競合シミュレーションは「resolve→差し替え→read」の
   分離単体テスト4件で検証 (シンボリックリンク差し替え・中間ディレクトリ
   差し替え・包含再検証自体・正常系回帰)。
2. **F2 devブリッジのゲート強化** (`src-tauri/src/lib.rs`, `catalog.rs`, `thumbnails.rs`, `devtest.ts`):
   `GENTO_DEV_BRIDGE` は値が**厳密に `"1"`** のときのみ有効 (`0`・`true`・空文字は無効)。
   `AppState` に起動時確定の `AtomicBool` を持ち、
   `set_failure_injection` / `devtest_report` / `dev_bridge_enabled` は
   無効ならErr (releaseで常に登録されていてもWebViewから呼べない)。
   コマンド本体は `*_impl` 関数に切り出して単体テスト可能にした。
   実機確認: releaseで `GENTO_DEV_BRIDGE=0` 起動時 `devtest:` 行0件。
3. **F3 URLバージョンの内容感応化** (`gento-image/src/cache.rs`, `src-tauri/src/{catalog,thumbnails}.rs`, `protocol.ts` 周辺):
   version = キャッシュキー入力と同じ (canonicalパス+size+mtime+アルゴリズム)
   のXxh3-128下位56bit (`content_version`。56bit ⇒ 最大17桁でクエリ18桁上限に収まる)。
   **設計判断**: サイズクラスは入力に含めない (URLパスがクラスを区別するため。
   DTOがクラス別の値を運ぶ必要を避ける)、cache_generationとの合成もしない
   (version単体で内容を識別、クリア後再生成は同一URL=同一内容で正当)。
   `thumb:ready` に `version` フィールド、ページDTO (`src-tauri` 層の
   `CatalogItemDto`。gento-coreはスコープ外のためthin adapterで付与) に
   `version` を追加。フロントの `ReadyMap` は世代ガード (cache_generation) と
   URL構成 (version) を分離。original表示はページDTOのversionを使う
   (生成不要)。devtest.tsヘッダコメントを実際の仕組みに修正。
4. **F4 2GiB会計の書き込み中ギャップ** (`gento-image/src/cache.rs`):
   `commit` は書き込み開始前に予定サイズをinflightへ予約し、予約ごとに
   enforceを走らせる。退避しても抑えられない予約は巻き戻して書き込みを
   失敗させる (バックプレッシャー。実運用では4ワーカー×64MiB上限で
   2GiBに遠く及ばないため通常通らない)。並行書き込み中の会計サンプリング
   テストで total ≤ max を検証。
5. **F5 cache.lookup の契約修正** (`gento-image/src/cache.rs`):
   `fs::metadata` (symlink追従) → `fs::symlink_metadata`。
   red→greenで実施 (修正前はsymlinkがHitになった)。

### 文書・証拠修正 (F6〜F10)

6. **F6**: `.gitignore` に `!docs/dev/measurements/logs/*.log` を追加し既存21
   ログをgit追跡対象化。`README.md` の `/tmp/opencode/wp7/` 参照を
   `docs/dev/measurements/` へ修正。
7. **F7**: `drivers/fg1-remeasure.py` で s256全件 (空キャッシュ) ×3 と
   preview2048全件 (空キャッシュ) ×3 をreleaseで再計測し
   `logs/s256-r{1..3}.log` / `logs/preview-r{1..3}.log` を保全
   (results.jsonに `fg1_*` キーで追記)。§2 S2補足・§4へ反映。
8. **F8**: 同ドライバで5万件仮想カタログのPage_Down 40連打 ×3 を計測し
   `logs/v50k-scroll-r{1..3}.log` を保全。§2 S3補足へ反映。
9. **F9**: `docs/dev/api-contract.md` — §14 rateはクランプ→**reject**に修正、
   `?v=` を内容感応version仕様に更新 (§10追加)、devブリッジ3コマンドの
   ゲート説明を追加、CSPは「img-srcのみの限定対応」と明記。
10. **F10**: 結果文書の証拠主張調整 (§1生ログgit追跡済み、§7表プロトコル行に
    TOCTOU対策の根拠、S3にltObs=0注記、S7に生成済み件数の出所注記)、
    ハンドオフの日付表記統一 (本書とWP-7ハンドオフの表題)。

## テスト

- `cargo fmt --all -- --check` — **PASS**
- `cargo clippy --workspace --all-targets -- -D warnings` — **PASS**
- `cargo test --workspace` — **PASS 159件** (gento-core 26 / gento-image 73 (+4) / gento-tauri 60 (+10))
- `cd apps/desktop && pnpm check && pnpm test && pnpm lint && pnpm build` — **PASS**
  (test 218件 (+2)。check警告7件はWP-5由来で不変)
- release build (`pnpm tauri build --no-bundle`) — **PASS**。再計測はこのバイナリで実施

## 再計測結果 (2026-08-30, release, ドライバ `drivers/fg1-remeasure.py`)

| シナリオ | r1 | r2 | r3 |
|---|---|---|---|
| s256全件 (空キャッシュ) 完了 | 10.05s | 10.05s | 10.05s |
| s256 p50/p95/p99 | 18.5/28.0/33.5ms | 18.6/27.5/33.6ms | 18.9/29.1/34.8ms |
| preview2048全件 完了 | 27.05s | 27.05s | 27.05s |
| preview2048 p50/p95/p99 | 59.3/89.6/106.0ms | 58.6/88.0/101.1ms | 59.1/86.9/100.2ms |
| v50kスクロール (40連打) long frame増加 | 2 | 2 | 2 |
| v50k 要求/受領/生成/キャンセル | 770/770/0/0 | 同左 | 同左 |
| v50k DOM / 停止後キュー | 63 / 0 | 63 / 0 | 63 / 0 |

preview再計測にはグリッド初期需要のs256 63件が混在 (生成カウンタ1,678)。
詳細は結果文書§4の注記参照。

## 設計判断の記録

- **versionにクラスを含めない / generationと合成しない**: 上記3.参照。
  api-contract §10に文書化。
- **予約失敗で書き込みを諦める (F4)**: ピンだらけで退避できない場合の
  唯一の不変条件維持手段。ジョブはfailedになりバックプレッシャーとして
  働く。通常運用では到達しない。
- **`*_impl` 関数への切り出し (F2)**: `State<'_, AppState>` を取る
  #[tauri::command] はTauriランタイムなしで呼べないため、ゲートと本体を
  `&AppState` を取る関数へ切り出して単体テスト可能にした。

## 残リスク

1. F1: 同一inodeへのhardlink差し替えは検出不能 (キャッシュdir書き込み権限
   持つ攻撃者は直接書き換え可能なため脅威モデル外。protocol.rsのコメントと
   結果文書§7に注記)。
2. F3: ページDTOのversion計算は項目ごとにcanonicalize (1 syscall/項目)。
   5万件仮想で1ページ500件でも数十ms以内に収まるが、MVPではレコード側での
   キャッシュ化を検討する価値がある。
3. F7 preview再計測のパーセンタイルはs256混ざりを含む (96%がpreview)。

## 次の担当者メモ (PM向け)

- コミットはPMが実施 (git add -n で logs/*.log が追跡されることを確認済み)。
- 再計測の再現: `python3 docs/dev/measurements/drivers/fg1-remeasure.py`
  (DISPLAY=:1, xdotool必須。キャッシュは `/tmp/opencode/fg1/` を使用)。
