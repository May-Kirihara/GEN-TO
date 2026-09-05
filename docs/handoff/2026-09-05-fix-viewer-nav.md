# FIX-VIEWERNAV-1 ビューア‹ ›送りボタンが実クリックで動作しない不具合修正 — 実行記録 (2026-09-05)

- WP: FIX-VIEWERNAV-1（ビューア送りボタン・実クリック不発火）
- 基盤: commit `61fae22` + 本WP変更 (vitest 673 → **676** / cargo 973 変更なし)
- 変更範囲: `ImageViewer.svelte` / `ImageViewer.test.ts` / `devtest.ts` /
  `uiux1_e2e.sh` / 本ドキュメント

## 結論サマリ

`.viewer-stage` の `onPointerDown` が button=0 で無条件に
`setPointerCapture` を呼ぶため、**stageの子要素**である送りボタン
(`.viewer-nav`) 上の実クリックが発火しなかった。stageがpointerをcapture
すると pointerup/mouseup が stage へリターゲットされ、mousedown対象
(button) と mouseup対象 (stage) の不一致により **click は最近共通祖先
(stage) で発火**し button の onclick に到達しない (Pointer Events /
UI Events の規定動作)。修正は pointerdown 冒頭でインタラクティブ子要素
(`button, a, input, select, textarea`) をパン対象から除外する早期return
のみ。E2Eが本bugを素通りした原因 (合成 `.click()` はpointer配列を経由
しない) を踏まえ、devtest に実クリック配列ヘルパ `realClick` を導入し
navボタン検証を実配列シミュレートへ切替えた。

## 根因 (実装で確認)

1. 実クリック: navボタン上の pointerdown (button=0) が stage へバブル →
   stage の `onPointerDown` が `drag` を設定し `stage.setPointerCapture
   (pointerId)` を呼ぶ → 以降の pointerup/mouseup は **stageにリターゲット**
   される → mousedown対象 (button) ≠ mouseup対象 (stage) → click は両者の
   最近共通祖先 (stage) で発火 → **button の onclick は不発火**
2. 合成 `.click()`: pointerdown/up を経由しないため button の onclick が
   直接発火 — **QA/E2Eが緑だった理由** (検証の盲点)
3. 画像面 (`.layer` は `pointer-events: none` のため実targetはstage自身)
   はdown/up対象が一致し click も stage で完結 — 従来操作には影響なし

## 修正内容

- `ImageViewer.svelte` `onPointerDown`: 冒頭で
  `(event.target as Element).closest("button, a, input, select, textarea")`
  が非nullなら早期return (drag開始もcaptureもしない)。ボタン類はstageの
  パン/ズーム操作に参加させない意図を明確化
- `onPointerMove` / `onPointerUp` の `drag === null` ガードは既存
  (確認済み) — pointerdownをskipした後にmove/upが来ても安全
- 画像上でのパン・ダブルクリック挙動は不変 (回帰防止テストで表明)

## テスト: 実クリック配列の再現 (red→green)

happy-dom は `setPointerCapture` を記録するだけでリターゲットを
シミュレートしないため、テストヘルパ `realClick` が**実ブラウザのクリック
意味論を合成**する: pointerdown (bubbles) で祖先がcaptureを呼んだ場合
(=patchで記録) は pointerup を capture要素へリターゲットし、click は
down/up両対象の**最近共通祖先**で dispatch。素朴な `.click()` はこの
経路を再現できない。

| テスト (ImageViewer.test.ts 新規3件) | 旧実装 (red) | 新実装 (green) |
|---|---|---|
| navボタン実クリック配列でcaptureせずnav(+1/-1)が発火 | **FAILED** (`captured=true`) | ok |
| navボタン経由pointerdownはドラッグを開始しない (skip後のmove/up安全) | **FAILED** (centerX 0.5→0.4375 = ドラッグ発生) | ok |
| 画像面 (stage) pointerdownは従来どおりcapture開始+パン+up終了 | ok (回帰防止) | ok |

- red実行: 旧実装のまま `676 tests: 2 failed / 674 passed` (上記2件)
- green実行: `676 passed (61 files)` — 既存wheel/キー/ドラッグ系全緑維持

## E2E: 実配列シミュレートの導入 (uiux1)

- `devtest.ts` に `realClick(target)` ヘルパを追加 (dragSequenceと同じ
  規約: 操作中のみ `Element.prototype.setPointerCapture` をpatch —
  合成pointerIdへのNotFound回避 + capture呼出の記録)。
  `viewerNavPrevBtn` / `viewerNavNextBtn` を `.click()` から realClick へ
  切替え (他のボタンアクションは不変)
- **E2E red実証**: 旧実装 (ImageViewer.svelteのみ一時revert) + realClick
  でビルドし uiux1 を実行 → `FAIL: 15) 次ボタン不全 (idx=0 xvd=1)` /
  RESULT **71/72** — 本bugクラスをE2Eが検出できることを確認
  (※「前ボタン PASS」は次ボタン不発火でidx=0既状態の自明一致)
- **E2E green**: 修正復元ビルドで uiux1 **72/72** (navボタン2条件は
  実配列経由で通過)
- `uiux1_e2e.sh` はセクション15のコメントに実配列シミュレートである
  旨を明記 (スクリプトの検証手順自体は不変 — devtest橋の切替えで実配列化)

## 教訓: 合成.clickと実pointer配列の乖離

- `.click()` / dispatchEvent("click") は pointerdown/up を経由しないため、
  **pointer capture系のbug (クリックが壊れる系) を構造的に検出できない**。
  「本物のDOM要素を叩いている=生産経路」でもイベント配列が実物と異なれ
  ば別の意味論を検証しているに過ぎない
- 実配列を合成する際はブラウザの規定動作 (capture時のupリターゲット・
  clickはdown/up対象の最近共通祖先) をヘルパ側に再現する必要がある
- 同一パターン (`setPointerCapture` を子ボタン持ち面で無条件呼出) は
  `CompareView.svelte` のslotにも存在するが、slot内にボタンが無く顕在化
  していないため本WPでは不改修 (要観測。slot内へインタラクティブ要素を
  追加する際は同一の除外ガードが必要)

## 検証

- vitest: 673 → **676** (全緑。`pnpm run test`)
- `pnpm run check`: **0 error** / 12 warnings (警告数は基盤 61fae22 と
  同数 — 全て前置き)
- `pnpm run lint`: 0 problem
- `pnpm build`: 成功 / `cargo build --release -p gento-tauri --features
  custom-protocol,devtools`: 成功 (E2E用)
- E2E uiux1: **72/72** (修正版) / 71/72 (旧実装red実証 — 上記)
- cargo test: 未実行 — Rustコード無変更のため (release buildで全crate
  コンパイルは確認)

## 残課題 / next steps

- (任意) CompareView slotへインタラクティブ要素を追加する際の同ガード
  (現状は顕在化なし)
- (任意) devtestの他ボタンアクションのrealClick全面置換 — 本WPは
  navのみ (委任仕様どおり)
