# UIUX-6 計画書 — 次バッチ候補の一括対応 (UIUX-5 follow-ups)

日付: 2026-10-07 / PM作成 / 対象: `apps/desktop` (フロントエンドのみ。Rust無変更)
元依頼: ユーザー「次パッチ候補の内容をすべて修正したい」。候補正本 = `docs/handoff/2026-10-07-uiux-5.md`「既知の問題 / 次バッチ候補」。

## 候補と対応方針 (対応表)

| # | 候補 (handoff記載) | 対応 |
|---|---|---|
| 1 | Minor-1: ビューアオープン失敗エラーがトーストのみ (4秒で消滅・常駐経路なし) | **WP1で修正** — エラーをnotice-stripへ常駐化 |
| 2 | popoverがチップ行に重なる (anchor分離は後続課題) | **実測で非問題と確定 — 変更なし** (下記「事前検証」) |
| 3 | `enqueueAiForSelection`通知が2.5s auto-clear扱いに変わった | **変更なし** (許諾済みの挙動統一。AI投入結果は短命情報でありtoast 4秒+popover 2.5秒で十分。常駐化は却下 — 完了通知がstripに溜まり続ける方がゴチャつく) |
| 4 | InfoPanel triage「採用 (P)」「除外 (X)」がmode bar表記と異なる | **WP1で統一** — 「採用」「除外」+titleヒント |
| 5 | トークン正規化 (最頻値統一・視覚変化あり・visual review要) | **WP2で実施** — font-size 10→5段階・radius 6→5。before/after視覚検証込み |

## 事前検証 (2026-10-07、実ブラウザ測定)

**候補#2は非問題**: `pnpm dev` + ブラウザでpopover開+チップ生成時の`getBoundingClientRect`実測 —
チップ行 bottom=375.26 / popover top=379.26 (**重なりなし**)。チップ行は`.search-bar`内の折返し2行目のため`top:100%` (bar下端) は既にチップ下。UIUX-5/WP2実装者リスクメモの記載は実装途中の一時状態に基づく誤記と判明。**コード変更せず本計画書+handoffでクローズ**する。証拠: `.playwright-mcp/uiux6-before-01-shell-popover-light.png`。

## スコープ

- WP1: viewer error常駐化 + InfoPanel triageラベル統一
- WP2: font-size/radius トークン正規化 (+視覚検証)
- やらないこと: Rust側 / spacing トークンの統一 (差分≤0.05remで視覚効果が小さくリスクのみ大きいため見送り — 必要なら将来個別評価) / 色トークン (WP1で既に同一値のみ統一済み)

## WP1 — viewer error常駐化 + triageラベル統一

- **viewer error常駐**: Appに`viewerError: string | null` stateを新設。
  - `openViewerForChoice`冒頭で`viewerError = null`クリア (旧UIUX-5前のviewerNoticeクリア挙動と同タイミング)。
  - catch節: `showViewerNotice(toErrorMessage(error))` (toast) → **`viewerError = toErrorMessage(error)`に変更** (toast pushなし — fileOpsErrorと同一パターン: エラー=strip常駐 / 案内=toast)。
  - ガイド («グリッドで項目を選択してください» = `openViewerFromToolbar`) はtoastのまま (非エラー)。
  - `NoticeStrip`に`viewerError: string | null` prop追加、`<p class="toolbar-notice error" role="alert" data-testid="viewer-error">`として描画 (**fileOpsErrorと同様・`.toolbar-notice.error`契約自然準拠 — xerr=1になるのはエラー表示中のみで正しいsemantics**)。
  - E2E互換: driverに`xerr=0`assert無しを確認済み (grep 2026-10-07)。新testid `viewer-error`は将来用。
- **InfoPanel triageラベル**: `src/lib/shell/InfoPanel.svelte`の「採用 (P)」「除外 (X)」(triageセクション内`[data-meta="cull"]`ボタン) → 「採用」「除外」+ `title="ビューアの高速選別モードでは P / X キーでも同じ操作ができます"` (SessionModeBar/WP12と同一様式)。
  - E2E互換: devtest `metaPick`/`metaReject`は`[data-meta="cull"]`スコープ内`startsWith("採用"/"除外")`照合 — 「採用」「除外」でもprefix一致維持。driver文言依存なし (grep確認済み)。

## WP2 — font-size / radius トークン正規化

**mapping (使用実績 2026-10-07集計に基づく)**:

| 現行 (uses) | → 正規化後 | drift |
|---|---|---|
| font-size-66 (3) | font-size-70 | +0.04rem |
| font-size-68 (6) | font-size-70 | +0.02rem |
| font-size-70 (12) | font-size-70 | 0 |
| font-size-72 (35) | font-size-75 | +0.03rem |
| font-size-75 (43) | font-size-75 | 0 |
| font-size-78 (51) | font-size-80 | +0.02rem |
| font-size-80 (30) | font-size-80 | 0 |
| font-size-82 (5) | font-size-85 | +0.03rem |
| font-size-85 (25) | font-size-85 | 0 |
| font-size-100 (14) | font-size-100 | 0 |
| radius-md (6) = 0.4rem | radius-sm (0.375rem) | −0.025rem |

- 計: font-size 10→**5段階** (70/75/80/85/100)、radius 6→**5段階** (xs/sm/lg/xl/pill)。
- 手順: ①`app.css`のtoken定義を正規化後のみに更新 (定義削除=66/68/72/78/82/md) ②全`.svelte`の`var(--font-size-*)`/`var(--radius-md)`をmapping tableどおり機械置換 ③`grep`で旧token参照0件を検証。
- **spacing/colorトークンは変更しない**。
- **視覚検証 (必須)**: 正規化前後で同一状態のスクリーンショット比較 (light/dark × shell/search popover展開+チップ/設定dialog)。before採取済み: `.playwright-mcp/uiux6-before-0[1-5]-*.png`。after採取後、PMが目視+差分確認 (レイアウト崩れ・意図しない折返し・切れなしことを確認)。

## 制約 (UIUX-5から継続)

- devtest/E2E互換: `.toolbar-notice.error`存在契約 / testid保存 (`search-filters`等) / button文言変更時の`clickButton` prefix+scope grep (WP1は確認済み) / `.sort-label select`・viewer choice文言等。
- 既存711 test全green維持 / svelte-check 0 error・12 warn維持 / eslint 0 / build成功。
- E2E 3種 (uiux1・qa_wp4・wp6) 最終green。

## テスト戦略

- WP1: NoticeStripのviewer error描画は既存コンポーネントテスト土台なし — App結線は型check+E2E+実機ブラウザで担保 (UIUX-5と同水準)。mapping純粋関数のような新規ロジックなし。
- WP2: 機械置換+grep検証+視覚比較+全テスト/E2E。

## 実装順序

WP1 (小修正) → WP2 (正規化) → 全検証 (vitest/build/check/lint + E2E 3種 + after視覚比較) → Codex final + QA → docs (README差分があれば/handoff/計画書完了記録) → commit (ユーザー依頼時)。

## リスク・assumption ledger

| assumption | basis | falsification | blast radius |
|---|---|---|---|
| popover/chips重なりは非問題 | 実測 rect (375.26 vs 379.26) | 別環境で再現? | なし (変更しない) |
| font-size正規化drift (max +0.04rem) は視覚的に安全 | 全drift ≤4%・最小分類のみ | before/after比較で崩れ | 全UI文字 |
| viewer error strip追加はxerr契約安全 | xerr=0 assert不存在grep | E2E失敗 | E2E gate |
| InfoPanel「採用」短縮はdevtest互換 | metaPick scope+startsWith実読 | E2E失敗 | E2E gate |

## 完了条件

1. WP1/WP2実装・vitest 711+ green・静的check基準値内・E2E 3種green。
2. before/after視覚比較でレイアウト崩れなし (PM確認)。
3. Codex final pass・QA (user-facing) pass。
4. handoff + 計画書完了記録 (+READMEにユーザー可視変更があれば反映 — WP1/2は README記載レベルの挙動変化なし想定)。

## 完了記録 (2026-10-07)

**完了 (done-with-notes)**。WP1-WP3実施 (WP3 = Codex final blocker修正: viewer errorのtoast併用 — 全画面オーバーレイ z-100 がstripを覆うため、即時フィードバックはz-600のtoastで保証)。

- vitest 711/711 / svelte-check 0 error・12 warn / eslint 0 / build✓
- E2E実機 (最終状態): uiux1 74/74・qa_wp4 11項目・wp6 17項目 (※中断実行の残留プロセス起因flake 1回ずつを除外 — 清掃後完全pass)
- 視覚検証: ピクセル差分=タイポグラフィ変化のみ・DOM clip監査 (764/1600px) 0件
- Codex final: **approve** (2回目 — blocker 1件をWP3で解消) / QA: **pass-with-notes** (出荷条件=本docs作成で達成・実機手動確認2項目はhandoff記載の代替証拠で運用)
- 残: `viewer-error`のE2E driverなし (将来用testid) — 詳細は`docs/handoff/2026-10-07-uiux-6.md`
