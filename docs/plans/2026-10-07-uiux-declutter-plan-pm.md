# UIUX-5 計画書 — UIスリム化 (declutter) バッチ

日付: 2026-10-07 / PM作成 / 対象: `apps/desktop` (Svelte 5 + Tauri 2 フロントエンドのみ。Rust無変更)

## 背景・目的

ユーザー要望: 「UIがごちゃごちゃしてるのでスッキリ、見やすく使いやすくしたい」。
PM調査 (2026-10-07) による診断 — 要素数ではなく**「常に全部見えている」構造**が主因:

1. ツールバー1行目: パンくず+ソート+コレクション操作(最大7コントロール)+エクスポート+新規フォルダ+サイズ+ビューア4ボタンが常時並列 (`App.svelte:3142-3373`)
2. 検索フィルタ7コントロールが常時展開 (`App.svelte:3505+`)
3. 6種のnotice行が別々に積まれる (search/viewer/folder/collection/fileOps×2)
4. 右InfoPanel 8セクション全展開 (1365行)
5. ヘッダーにインジケータ6個+notice 2個
6. デザイントークン不在 (`app.css` 実質23行) → scoped CSS各コンポーネントで値が不統一
7. `App.svelte` 4644行モノリス

## スコープ

やること (ユーザー承認済み「全部」):

- **WP1 デザイントークン基盤** (B5+B6): `app.css` にspacing/font-size/color/radius/shadowトークン定義、全コンポーネントscoped CSSをトークン参照へ移行
- **WP2 検索UI整理** (A1): フィルタ7コントロールをポップオーバー化、適用中条件をチップ表示
- **WP3 ツールバー整理** (A3+D9+D10): ビューア選択セグメント化・サイズ2値トグル化・コレクション位置操作のオーバーフロー化・toolbarエクスポートボタン撤去
- **WP4 通知統合+ヘッダ簡素化** (A2+A4): notice 6行→1行ストリップ+トースト、ヘッダーインジケータ統合
- **WP5 InfoPanel整理** (C7): 8セクション→アコーディオン (既定展開=ファイル情報+評価・選別)
- **WP6 選別セッション モードバー** (C8): セッション中はグリッド上部に専用バー (採用/除外/前へ/次へ/終了+進捗)
- **WP7 App.svelte分割** (E11): toolbar/search/notice/headerをコンポーネント抽出 (状態はApp残置・props/eventsのみ。純粋抽出)

やらないこと (非ゴール):

- Rust側 (crates/*, src-tauri) の一切の変更
- 新機能追加 (テーマ切替UI、新キーバインド等)。トークンは`color-scheme: light dark` (=システム追従) のまま
- ペイン構成 (3ペイン)・左ペインタブ構成の変更
- 検索構文・DB・設定スキーマの変更
- E2E driver (uiux1_e2e.sh) の検証項目削減 (読み出し経路整合のみ更新可)

## 制約 — 絶対維持事項 (E2E・既存テスト互換)

> **改訂 (Codex plan反映 2026-10-07)**: 初版より互換インベントリを拡充し、
> WP1/WP4/WP5/WP6/WP7の契約を修正した (詳細は各WP節と「改訂メモ」)。

1. **`devtest.ts:1728` の `xerr` は `document.querySelector(".toolbar-notice.error")` を読む** → 通知統合後も**活性エラーは必ず生DOMに存在**すること (履歴popover内だけに追いやらない)。readoutキー (`xerr/xsp/xes/xdlg/...`) の意味は不変。
2. data-testid保存 (移設時もtestidは追従。DOM内位置は可変、存在と意味は不変):
   - 検索: `search-bar` / `search-toggle` / `search-summary` / `search-summary-query` / `search-scope-badge` / `search-summary-clear` / `search-clear` / `search-input` (**`search-bar`内に維持** — devtestのsrFocusInputが依存) / `search-run` / `search-filters` / `search-notice` / `search-semantic-value`
   - fileops: `fileops-notice` / `fileops-error` / `fileops-progress`
   - パンくず: `breadcrumb-path-button` / `breadcrumb-path-input`
   - InfoPanel: `search-evidence` / `semantic-score` / `similar-search-button` / `ai-runs` / `settings-about`
3. **devtest.tsのDOM駆動インベントリ** (ボタン文言・class・selector依存 — 変更時はdevtest.ts+E2E driverを同時更新、ただし極力不変維持):
   - `.sort-label select` (cycleSort) / viewer choiceボタン文言「単一」「2枚比較」「4枚比較」+`.active` class (cycleChoice) / 「開く」 (openViewer)
   - セッション: 「現在の一覧で開始」「再開 (」「次へ →」 (sessionStartFolder/sessionResume/sessionNext — InfoPanel内)
   - `data-meta` 系selector群 (rating/cull/undo/tags/caption等 — InfoPanel内)
   - `data-meta-section` (photo/generation/raw) は**折りたたみ時もDOMに存在**すること (assetMetaLines読み出しが`querySelector(...) !== null`で判定)
   - `.picker-backdrop` (xdlg) / `.dev-drawer` (xdd) / `[role="dialog"][aria-label="AI設定"]` (xai)
4. エクスポート導線: MenuBar「ファイル>SD学習用エクスポート…」が正本 (E2Eが使用)。toolbar側ボタンは撤去可。
5. レイアウト永続化 (`layout-prefs`) は**明示的フィールド列挙型** (type/default/parse/serialize全変更が必要)。WP5の新フィールドはこの契約に従い、`saveLayout()`呼び出しは既存フィールドを失わないmergeであること。破損時・未登録時は既定へフォールバック (UIUX-3 V-1の4パターン踏襲)。
6. セッションAPI (`selection-session.ts`) の公開署名不変 (WP6は消費側のみ)。`sessionSnapshot` の形状は `{active, origin}` (`current`フィールドは存在しない)。
7. キーボード操作 (メニュー矢印/Esc、triage-keys、検索折りたたみの永続化) の挙動不変。

## アプローチ詳細 (WP別)

### WP1 デザイントークン (改訂: 値保存的導入に縮小)
- `app.css` `:root` にトークン定義: spacing scale / font-size scale / color (light/dark両対応。現行`light-dark()`対の**実値そのもの**を収集) / radius / shadow。
- **本WPは値保存的置換のみ**: 既存scoped CSS内のリテラル値と同一の値を持つトークンへ置き換える (例: `light-dark(#d0d7de, #444c56)` が複数箇所 → `var(--color-border)` に集約、トークン値=その同一値)。**最頻値への正規化 (見た目変化を伴う統一) は行わない** — それは別review (visual確認) を要する後続作業とする。
- 対象: src配下`.svelte` 25ファイル (App.svelte含む)。同一値の重複が複数箇所あるものを優先し、1回しか出現しない文脈固有値はトークン化しない (無理な共通化をしない)。

### WP2 検索フィルタ ポップオーバー+チップ (改訂: 実態に合わせ縮小)
- 現状: 展開検索バー内にフィルタトグル (App.svelte:3476付近) があり`search-filters`をインライン表示している。**新規作業は**: ①フィルタ群をpopover (絶対位置配置・Esc/外クリックで閉じる) へ移設、②適用中フィルタのチップ表示、③閉時挙動。
- チップは**ドラフト値** (`searchFilters`現値) を反映 — 編集UIの一部として。折りたたみ行のsummaryは**確定値** (lastSearchMeta) のまま不変 (UIUX-3 V-3契約)。この2層区別を明示。
- チップ文言: `★3以上` / `採用` / `♥` / `PNG` / `タグ:xxx` / `追加日 a..b`。×で当該フィルタ解除 (ドラフト更新)。
- `search-input` は `search-bar` 内に維持 (制約2)。`search-filters` testidはpopover内へ移設。

### WP3 ツールバー整理 (改訂: 規模縮小 — グループ化+スタイルが主)
- ビューア選択: 既存3 choiceボタン (単一/2枚比較/4枚比較) +「開く」→ 見た目をセグメントコントロール化 (連結ボタン・`.active`維持)。**文言と`.active` classは不変** (制約3)。
- サムネイルサイズ: `min=0 max=1 step=1` スライダー → 「小/大」2値トグル (`sizeSliderValue`保存値・`thumbSize`永続化意味不変)。
- コレクション手動表示時: 位置操作group →「…」オーバーフローボタン1個のpopoverへ集約 (文言・disabled条件・title維持)。
- toolbar `export-entry` 撤去 (MenuBarが正本)。「+ 新規フォルダ」維持。スマート「再評価」維持。

### WP4 通知統合+ヘッダ簡素化 (改訂: エラー常駐契約)
- 6 notice行 → 1行`notice-strip`: **エラーを最優先で表示**、次にstatus。**活性エラーは常に生DOMに存在** (`.toolbar-notice.error` + `data-testid` + role維持 — 制約1/2)。履歴popoverはエラーを除外してよい。
- 各notice要素のtestid (search-notice/fileops-notice/fileops-error) とrole (alert/status) は個別維持。`fileops-progress` は進捗バー常設維持。
- 揮発性の非エラーnotice (fileOps完了等) はトースト (数秒自動消滅) にしてよい — ただしfileops-notice testidを持つ要素は存続中表示中は残す。
- ヘッダー: scan-indicator×3+warn-indicator → ステータスアイコン1個 (状態色) +詳細popover (各インジケータ情報・metaNotice/osNoticeを格納)。health-dot・source-badge・title維持。

### WP5 InfoPanel アコーディオン (改訂: DOM常駐+安定ID+merge契約)
- **7セクション** (一部条件付き表示) をアコーディオン化。各セクションに**安定ID** (`section-file` / `section-meta-photo` / `section-meta-generation` / `section-meta-raw` / `section-triage` / `section-memo` / `section-session` 等) を付与。
- **折りたたみ時もセクション内容はDOMに常駐** (CSS hidden。LeftPaneTabsの「全パネル常駐+hidden」precedent踏襲) — 理由: assetMetaLinesの`data-meta-section`存在判定・devtestの`data-meta`/セッション系ボタン駆動がDOM前提 (制約3)。hidden中はfocus不可 (tabsと同一モデル)。
- 既定展開: 「ファイル情報」「評価・選別・タグ・キャプション」+「選別セッション」(活性時)。他は折りたたみ。セッション開始導線 (「現在の一覧で開始」「再開 (」等) の到達性はこの既定+DOM常駐で保証。
- 永続化: `layout-prefs`へ新フィールド (例: `infoPanelCollapsed: string[]`)。**契約**: type/default/parse/serialize/`saveLayout()`呼び出し側を一体更新。未知のセクションIDは無視 (前方互換)、フィールド欠損・破損時は既定展開へフォールバック。`layout-prefs.test.ts`に4種パターン (キー無し/フィールド無し/保存値保持/破損) 追加。
- セクション内DOM・testid・`data-meta-section`不変。

### WP6 選別セッション モードバー (改訂: Appラッパ経由に修正)
- 表示条件: `sessionSnapshot.active === true`。
- グリッド上部 (notice-stripの下) にモードバー: 「◀ 前へ」「採用 (P)」「除外 (X)」「次へ ▶」「終了」+ `sessionCounts`件数表示。
- 前へ/次へ = App既存 `sessionMove(±1)` (フォーカス同期込み)。終了 = App既存 `endSession()`。**storeの直接呼び出しはしない**。
- 採用/除外 = **App層の新アクション** `sessionTriage(value: "picked" | "rejected")`: ①対象=`session.currentId()`、②編集可否=Library表示 && `gridPagerHasId(id)` && dbState==="asset" (handleTriageActionと同一基準)、③`applyCull([id], value)`をawaitし**成功時のみ**`sessionMove(+1)` (失敗時は`sessionNotice`にエラー・位置不変)、④triage-keys (P/X) と同じ操作semantics。
- InfoPanel「選別セッション」セクションは開始/再開導線として維持。
- 注意: ビューア内triage (`handleTriageAction`) は`viewerStore`由来のため本バーは流用しない (別経路としてApp層に新設)。

### WP7 App.svelte分割 (改訂: 性質明確化+ゲート強化)
- 抽出: `AppHeader` / `Toolbar` / `SearchBar` / `NoticeStrip` (WP4成果) / `SessionModeBar` (WP6成果) を`src/lib/shell/`へ。
- 性質: **純粋抽出ではない** — Svelte scoped CSSは抽出先へ移設が必要 (親のscoped selectorは子に効かない) + 型付きprops/callbacks設計。状態・storeはApp残置。
- **実施条件 (gate)**: WP1-6完了+vitest全green+E2E green後のみ。E2Eで検証できない場合WP7はdefer (文書化)。

## テスト戦略

- **E2E baseline先行**: 実装前に`uiux1_e2e.sh`を現commitで実行し環境でのgreenを確認 (Xephyr/xdotool利用可を確認済)。
- 各WP: test-first (WP2/3/4/5/6の新挙動はコンポーネントテストを`*.test.ts`に先行追加。純粋ロジックは純粋関数へ分離し単体テスト — UIUX-1教訓)。
- 既存679 test (62ファイル) 全green維持を毎WP確認。
- 最終: `pnpm test` + `pnpm run build` + `pnpm run lint` + `svelte-check` (implementer実行・結果報告) + E2E `uiux1_e2e.sh` 再実行。
- 回帰検証ポイント: 検索折りたたみ永続化・メニューキーボード・triageキー・エクスポート導線・パンくずパス入力・`assetMetaLines`系readout。

## 実装計画・順序

WP1 (トークン) → WP2 → WP3 → WP4 → WP5 → WP6 → **E2E checkpoint** → WP7 (gate満たす場合のみ) → 最終gate (Codex final / QA / docs)。
各WP = implementerへの独立したbounded contract。PMはWP毎に構造検証+関連テスト実行。

## 改訂メモ (2026-10-07, Codex plan review反映)

- verdict は初版 **reject** → 本改訂でblocker 4件すべて修正:
  1. WP6の`sessionSnapshot.current`は不存在 → `active`+Appラッパ (`sessionMove`/`endSession`) +新設`sessionTriage`(成功時のみ前進) に修正。
  2. WP5の既定折りたたみがE2E駆動を壊す → 折りたたみ時もDOM常駐 (LeftPaneTabs precedent) +安定ID +layout-prefs merge契約。
  3. WP7の「純粋抽出」は不成立 (scoped CSS) → 性質明確化+E2E gate条件付き。
  4. WP4のxerr保持のみでは不十分 → 活性エラーの生DOM常駐+testid/role個別維持。
- 非blocker反映: WP1は値保存的置換に縮小 (正規化は後続) / WP2は既存トグル実態に合わせ縮小+draft/committed区別 / 互換インベントリ拡充 (search-clear等・devtest駆動文言) / ファイル数25。
- WP7はacceptance gateから分離 (gate: E2E green時のみ実施、不可ならdefer)。

## 見積り

- WP1: 広いが機械的。WP2-6: 中規模各1回。WP7: 機械的だが大型。

## リスク・assumption ledger

> 改訂 (2026-10-07 Codex反映): 初版の「トークン値=現行最頻値」「WP7純粋抽出」行は
> 改訂版アプローチ (値保存的置換 / 抽出+CSS移設+型付きprops) に置き換え。

| assumption | basis | falsification | blast radius |
|---|---|---|---|
| トークン値=置換元リテラルと完全同一 (値保存) | Codex最終reviewの機械検証: 674宣言が同一property/valueで一致・undefined token 0 | 目視で変化 | 全UI見た目 |
| `.toolbar-notice.error`保存でE2E xerr互換 | devtest.ts:1728 / E2E 74/74・qa_wp4・wp6実測 | uiux1_e2e失敗 | E2E gate |
| App.svelte抽出 (CSS移設+型付きprops) で挙動不変 | E2E 3種green・vitest 705 | テスト/e2e不一致 | 全UI |
| `layout-prefs`新フィールド (infoPanelCollapsed) はmerge契約で安全 | UIUX-3 V-1踏襲 + 4パターンテスト | layout-prefs.test失敗 | 設定永続化 |
| metaNotice/osNoticeはtoast化で視認性回復 (WP8) | Codex blocker指摘→$effect toast mirror | 実機でtoast不表示 | アクションフィードバック |
| UIUX-5というバッチ名 (handoff番号衝突回避) | docs/handoff/2026-09-05-uiux-4.md はUIUX-3のhandoff | なし (命名のみ) | なし |

## 完了条件

1. 上記スコープ7WP実装・全vitest green・build/lint/svelte-check基準値内 (error 0)。
2. E2E互換 (実施または文書化された手動確認項目)。
3. README「製品UIシェル」節・handoff文書更新。
4. Codex最終review pass。QA (user-facing) pass。
5. PM最終diff検査。

## ロールバック

UI専用・WP毎にgit commit分離 (implementerはcommitしない — PM指示待ち。実際はユーザーcommit方針に従う)。WP単位のrevertで復旧可能な構造で進める。

## 完了記録 (2026-10-07)

**完了 (done-with-notes)**。実装 WP1-13 (Codex final blocker修正3 WP + QA指摘小改修2 WP込み)。

- vitest 711/711 (baseline 679+32) / svelte-check 0 error・12 warn (baseline同一) / eslint 0 / build✓
- E2E実機 (最終状態で再実行): uiux1 74/74・qa_wp4 11項目・wp6 17項目
- Codex final: **approve** (4回目 — 通知トーストのblockerをWP8-10の3段で解消)
- QA: **pass-with-notes・出荷可** (出荷条件2件=docs完成・gitignoreは対応済み)
- follow-ups (次バッチ候補): ビューアエラー常駐経路 (QA Minor-1) / チップ行とpopoverの重なりanchor分離 / トークン正規化 (visual review要) — `docs/handoff/2026-10-07-uiux-5.md` 参照
