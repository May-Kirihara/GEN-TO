# UIUX-7 — 意味検索モードUI改善 (3モードセグメント+サジェスト+準備完了通知)

日付: 2026-10-08 / PM計画・検証 / 実装: implementer / ユーザー承認: A+C+D構成

## 背景

EMBED-SWEEP-1 (c05e6bf) 修復後も、意味検索の入口が連続スライダー (既定0=純キーワード)
のため「意味検索が動かない」と誤解しやすい (ユーザー実報告)。重みの連続値 (63%等) は
意味不明で、発見性も低い。→ スライダーを廃止し3モード化+気づき支援。

## スコープ (承認済み A+C+D)

### A. 3モードセグメント (スライダー置換)

- SearchBarの `search-semantic` ブロック (SearchBar.svelte:188-212) を**3択セグメント**へ
  置換: 「キーワード (0%) / ハイブリッド (50%) / 意味 (100%)」。UIUX-5のセグメントUI
  (ツールバー・モードバー) と同パターン・同デザイントークン。
- props: `semanticMode: "keyword" | "hybrid" | "semantic"` +
  `onSemanticModeChange(mode)` に変更 (`semanticWeightPercent`/`onSemanticWeightInput`廃止)。
  App側もmodeで保持し、実行時weightは `MODE_WEIGHT[mode]` で導出。
- `semanticAvailable` でなくても表示し**disabled+title=semanticReason** (UIUX-5の
  不可視理由tooltip契約維持)。利用可ならtitle=各モード説明。
- **modeをlocalStorageに永続化** (既存storeの慣行に準拠・破損値は検証してフォールバック。
  キー例: `gento.search.semanticMode`)。初回既定=keyword (現行互換)。
- 折りたたみ検索行の表示 (App.svelte:1382 `semanticWeightPercent` meta) は重み→
  ラベル変換で表示: 0=キーワード / 100=意味 / 類似検索(meta kind=similar)は「類似」。
- testid: container=`search-mode-segment`・各ボタン=`search-mode-keyword|hybrid|semantic`。
  aria-pressedで選択状態。

### C. 自然文サジェストチップ

- 純関数 `src/lib/ui/semantic-suggest.ts`:
  `suggestsSemantic(query: string, weightPercent: number, semanticAvailable: boolean): boolean`
  — すべて満たすときtrue: ①semanticAvailable ②weightPercent===0 (キーワードモード)
  ③query.trim()非empty ④**構文トークンを含まない** (`[A-Za-z_]+:` を含まない —
  tag:/rating:/type:等のフィールド構文入力中は出さない)。
- 検索**実行後**に判定し、SearchBar内 (警告行の近く) にチップ表示:
  「🧠 意味検索を試す」(`data-testid="semantic-suggest-chip"`)。入力中の逐次表示はしない
  (タイピングノイズ防止)。
- クリック → modeを`hybrid`に変更し**同じクエリで再実行**。チップは再実行後に消える
  (weight>0で条件②不成立)。

### D. 準備完了通知+プレースホルダ改善

- `vector-index:ready` 購読箇所 (App.svelte:2799) で**1回だけ** (localStorageフラグ,
  例 `gento.semanticReadyNotice.v1`) トースト通知: 「自然言語検索の準備ができました —
  検索バーのモードを「ハイブリッド/意味」に切り替えると使えます」。
  readyイベントは起動時の索引ロードでも発火するため、フラグで1回限りとする。
- 検索ボックスplaceholder (SearchBar.svelte:163) を自然言語例文入りに:
  「検索 (自然言語OK: 夕暮れの海岸 / 構文: tag:night rating:>=3)」。
  ※現行placeholder文字列・`search-semantic*` testidへのdrivers/devtest.ts参照は
  **ゼロ件を確認済み** (互換リスクなし)。

## 非対象

- weightLabel (src/lib/search.ts:144) のbackend契約・search_run API (weight 0..1は不変)。
- 意味検索本体・索引・E2E driver新規追加。B案 (既定ハイブリッド化) は見送り。

## テスト戦略 (test-first: 純関数)

- `src/lib/ui/search-mode.ts` (新規純関数): mode↔weight変換 (0/50/100)・ラベル変換
  (キーワード/ハイブリッド/意味/類似)・永続化値バリデーション (不正値→keyword)。
- `src/lib/ui/semantic-suggest.test.ts`: 上記4条件 (構文トークン除外「tag:猫」「rating:>=3」、
  空文字、weight>0、不可用)。
- SearchBarコンポーネントテスト (SessionModeBar.test.ts様式): 3モード描画・disabled+title・
  選択状態・チップ表示条件・クリックで`onSemanticModeChange("hybrid")`発火。
- 既存vitest全green・svelte-check・eslint・build。
- 既存E2E 3種 (uiux1/qa_wp4/wp6) green維持 (参照ゼロ確認済みだが実検証)。

## 検証

- `pnpm test`・`pnpm run check`・`pnpm run lint`・`pnpm run build` (apps/desktopで実行)。
- E2E 3種はrepo rootから個別実行 (孤立Xephyr/vite掃除後)。
- 素ブラウザ (`pnpm dev`) でセグメント/チップ/placeholderの目視確認 (PM)。

## 完了条件

1. 純関数テスト red→green (test-first)・コンポーネントテストgreen。
2. 静的check・build・E2E 3種 green。
3. Codex final review pass。
4. QA final (user-facing) pass。
5. README検索セクション記載整合 (スライダー→モード表現に更新要否を確認し必要なら更新)。
6. handoff + 完了記録。

## リスク評価

normal (フロントエンドUI変更・API不変・永続化追加のみ)。fast mode。

## 完了記録 (2026-10-08)

**完了 (done-with-notes)**。WP1 (実装) → WP2 (PM検収: keyword常時有効化) → WP3/WP4 (Codex blocker修理×2:
ready toast購読前発火対策=現在状態判定併用・チップ却下/running/searchActive条件) → WP5 (QA Major:
不可時理由をdisabledボタンtitleへ連結+chip aria-label)。

- **vitest 743/743** (新規32: search-mode/semantic-suggest/SearchBar) / svelte-check 0 error・12 warn (既存) / eslint 0 / build✓
- **E2E 3種 (最終releaseバイナリ・wp6はfresh storage)**: uiux1 **74/74**・qa_wp4 **11項目**・wp6 **17項目**
- **Codex final: approve** (round 3 — reject→reject→approve) / **QA: pass-with-notes** (MajorはWP5で解消済み)
- QA残留 (情報): 永続mode=意味が不可状態で「押下済み+disabled」に見える (実行はw=0フォールバック・badgeは実際の実行モード表示)、fetch-to-listen ms級race、chipのApp統合テスト不在 (純関数coverageのみ)
- QA手動確認リスト (実機): 結果の質・索引構築中→readyの実タイミング・Tab順序・ダークモードコントラスト — handoffへ記載

## 副次修正 (UIUX-7と同時実施・コミット対象)

`devtest.ts focusCollectionMemo`: UIUX-5のInfoPanelアコーディオン既定 (section-memo折りたたみ) で
`hidden` textareaへのfocusが無音no-opになり、wp6 E2Eメモステップが**ストレージ状態履歴依存**で
失敗するようになっていた (UIUX-5導入以来の潜在・fresh環境で必須失敗)。展開click→150ms→focusに修正。
**UIUX-7・EMBED-SWEEP-1は無関係** (コミットbisect c97da0e/e36c2f9で実証)。

### プロセス教訓 (E2E証拠の健全性)

1. **E2E実行前にtargetバイナリの新旧を確認** (mtime vs ソース変更) — 今日の切り分けで最初に迷った原因。
2. E2E driverのHOME/localStorageは**実行履歴に依存する結果**を生む — fresh状態での再実行も最終証拠に含めるべき。
3. UIUX-6検証時のwp6メモステップgreenは 展開済みlocalStorageが偶々残っていたためと推定 (確定不能・証拠消滅)。
