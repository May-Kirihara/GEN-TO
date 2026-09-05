# UI/UX改善4件（スクロール既定・開発統計移設・検索折りたたみ・ビューア送り）— PM計画

日付: 2026-09-05 / 実装: implementer単一WP / 正本: 本書 + `/tmp` レビュー

## 要求と設計方針

### V-1: ホイールスクロール既定2.0化＋スライダーを設定モーダルへ移動
- 現状: 既定1.0・toolbar「速度」スライダー・設定dialog表示タブは読取専用（「ツールバー『速度』で変更」の案内）
- 方針:
  - **既定値を2.0へ**。**保存済み値の取り扱い**: layout-prefsの`scrollSpeed`が「変更時のみ永続化」であることを実装時に確認——未保存ユーザー（キー無し）は2.0になり、明示的に設定したユーザーは保持。**もし初回ロードで1.0が先行保存される実装なら**: 保存値1.0を「未選択」と区別できないため、**保存値<2.0を2.0へ切上げる移行はしない**（ユーザーが1.0を意図選択した可能性を壊さない）——代わりに「既定に戻す」操作を設定UIに添える。実装時の実態を報告し採用
  - スライダーを設定dialog**表示タブ**へ移動（読取専用→編集可能・即時反映+変更で永続化。設定タブの規約「確認のみ」は这次、表示系prefを直接編集する例外とする根拠をdocに明記——既存cacheタブが上限変更を受け入れている前例あり）
  - toolbarのスライダーは撤去。E2E（uiux1速度系・`xsp=`）の入口を設定dialog経路へ更新

### V-2: 開発統計オーバーレイを開発パネルへ移動
- 現状: `ThumbnailGrid`の`grid-stats`（DOMセル数/可視row/overscan/列/total/scrollTop/選択/需要）が`showStats={devBridgeActive}`で主UIに常時描画
- 方針: **統計の計算はThumbnailGridに残し、表示はDevDrawerへ**。statsをcallback/propsでAppへlift（更新頻度が高いのでthrottleまたは表示時のみpull——devtestのG行と同じ粒度）。主UIのオーバーレイは削除。DevDrawerに「グリッド統計」セクションを新設。`showStats` propは削除しE2E依存（スクリーンショット解析でstats読み取るdriverがあれば）を棚卸し
### V-3: 検索フィルター折りたたみ
- 対象: toolbarの検索行（検索input+scope選択+意味重みスライダー等）
- 方針: 折りたたみトグル（chevron/「検索」ボタン）。**折りたたみ時**: 1行コンパクト（🔍+アクティブな検索語の切り詰め表示+scope badge。検索実行中は見えないと困惑するため）。展開時: 現行。状態はlayout-prefs永続化。キーボード: トグルはbutton・展開でinputへfocus
### V-4: ビューア送りボタン＋ホイール意味変更
- **左右端に送りボタン**: 半透明の大的確ヒットエリア（`‹`/`›`）・aria-label（前の画像/次の画像）・クリックで`store.nav(∓1)`・端の挙動は`nav`既存仕様に従う・hoverで強調・フルスクリーンでも表示。矢印キー既存と同一経路
- **ホイール意味変更**: 平常wheel=画像送り（deltaY符号でnav(±1)）・**Alt+Wheel=ズーム**（既存zoomAt）。`+`/`-`/`0`/`1`キー不変。 CompareView（2/4枚比較）のwheelは**変更しない**（ユーザー指定はビューア）——docsに明記
- **E2E影響（重要）**: devtestのviewer wheel系アクション（L2054 `wheelAt`、ctrlKey分岐あり）を棚卸し——ズーム検証はAlt付きへ、新規に「wheel送り」アクションを追加。driver側の該当検証更新

## 受け入れ条件
1. 新規状態（保存値なし）で速度2.0適用・設定dialog表示タブでスライダー編集・永続化・toolbarにスライダー無し
2. 主UIに開発統計オーバーレイ無し（devBridge有効時も）・開発パネルにグリッド統計表示・devtestのG行等E2E資産は壊さない
3. 検索行の折りたたみ/展開・状態永続化・折りたたみ中も検索語とscopeが見え実行中検索の解除が可能
4. ビューア左右端ボタンで送り・wheelで送り・Alt+wheelでズーム・比較ビュー不変・矢印/ズームキー不変
5. 既存E2E全緑（driver更新含む）・vitest/cargo基準維持超え・check 0 error

## テスト戦略
- vitest: 速度既定値/移行semantics・設定タブ編集・折りたたみ状態・ビューアwheel意味（Alt有無）・navボタン（component）・DevDrawer統計表示
- E2E: uiux1拡張（速度2.0初期値・設定dialog編集・ビューアwheel送り/Altズーム・送りボタン・検索折りたたみ）+ 既存driverのviewer wheel系更新と再実行
- red→green: 純粋関数・意味変更部で実証

## リスクと仮定
| 仮定 | 根拠 | 反証条件 | 爆発半径 |
|---|---|---|---|
| scrollSpeedは変更時のみ永続化 | UIUX-1実装 | 実装確認で先行保存発覚 | 「既定に戻す」操作で吸収 |
| grid-statsのE2E DOM依存は少ない | devtestはbridge経由 | driver棚卸しで発覚 | driver更新（小） |
| wheel送りは誤操作リスク低 | ビューアは画像単位文脈 | ユーザー違和感 | 設定化（次イテレーション） |

## 結果（2026-09-05完了）
- 実装 `b9aff9f` → Codex差分 **approve-with-notes**（blockerなし。notes=scope-kind hookが確定値でない/lastSearchMeta未消費フィールド/ThumbnailGridの旧既定1/generate wheel-navのdeltaMode未正規化→いずれも次イテレーション候補として記録）→ QA **pass-with-notes・出荷可（条件なし）**（要求4件すべて実機検証・vitest 673/cargo 973/uiux1 72/72独立一致・比較ビュー/triage/矢印不変確認）
- 付帯修正: devtest F4ハンドラの二重潜在bug（ctrl+F4がmetaRate5へ先行routing+wheelAtがctrlKey未伝播——wp16_s4_viewer.pyの主張が到達不能だった）を解消し橋アクション分離（素F1/F12モード）
- QA申し送り（次イテレーション候補）: wheel連続送りの体感（latch+250ms静穏で連続回転=1枚止まり。ボタンrepeat/静穏の速度連動）・triage中wheel誤送りガード・明背景での‹›視認性・README括弧typo・scope badge f/cのE2E追加・DevDrawer poll単体試験
- 環境注記: 本WP実装時にGaOTTTが実装者側から利用不可だった（PM側は正常——保存はPMが実施）

## 改訂（Codex plan反映 — /tmp/codex-uiux3-plan.md・実装契約）
1. **V-4 wheel**: ズーム=`Alt||ctrl||meta`+wheel（**pinch=ctrl+wheelをズームとして保存**）・平常wheel=送り（**accumulator+閾値+gesture cooldown/latch——trackpadの多発eventで複数枚飛びしない。意図的な1ノッチ/ジェスチャ=1送り**）・ビューアがジェスチャを握る間は常にpreventDefault
2. **V-2**: コールバックでAppの$stateへlift**しない**（毎フレーム再描画の嵐）。`getStatsSnapshot()`をexportし**DevDrawerが開いている間だけ250-500msでpoll**。`data-grid-cell`/`.grid-scroll`のDOMフックはE2E（G行）が使うため温存
3. **V-3**: 折りたたみ要約は**確定済み検索状態**（lastQuery/last-kind）から（`searchInput`でない）・類似画像は「類似画像」表示・**検索実行中はclear/exit操作を折りたたみでも見せる**・scope表示は実態名（「登録ルート: ‹名前›」「コレクション: ‹名前›」「ライブラリ全体」——「現在のフォルダ」は実装と不一致のため使わない）・フィルタ/意味重み有効表示・展開→focusはtick()後。`searchExpanded`（既定=展開）でlayout-prefsへ
4. **navボタン**: `nav()`は両端でclamp（非wrap）→**端でdisabled**・total≤1・loading考慮
5. **E2E**: `wheelAt`にmodifier options（alt/ctrl/meta）追加+**橋アクション分離**（viewer送り/viewer Altズーム/pinch/compareズーム）・**F4ハンドラの潜在bug修正**（ctrl+F4がmetaRate5へ先に路由されwheelAtのctrl分岐が到達不能——wp16_s4_viewer.pyの主張と不一致）・run_b.pyのF4使い分け追従・速度読み取りは「設定→表示を開いてから」の契約に更新（閉時-1受容）
6. **V-1**: layout-prefsは共有blob保存のため「scrollSpeed変更時のみ」ではない（他のlayout保存で現在値が書き込まれる）——**no-migration立場は維持**: キー無し/フィールド無し→2.0・保存1.0は保持・「既定に戻す」は明示2.0書込。テスト: キー無し/フィールド無し/保存1.0/破損値。SettingsDialogは**store所有権を与えずcallback（input即時/change永続化/reset）**で編集受付（cache タブの上限変更前例に倣う）
