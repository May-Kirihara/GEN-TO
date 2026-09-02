# DEVUX-1 ハンドオフ — devtest画面読み出しの既定非表示とトグル (2026-09-02)

実装者: implementer subagent。基盤コミット: f206702。
**devブリッジの画面読み出し (`G tot=...` 固定pre) がヘッダー右上に常時重なり、
devビルドで製品UIの視認を妨げていた問題の修正。**

## 変更内容

- `lib/devtest-readout.ts` (新設): 読み出し可視性の純粋ヘルパー。
  `parseDevReadoutVisible` / `toggleDevReadout` / `DEVTEST_READOUT_KEY`。
  `lib/ui/layout-prefs.ts` と同じ KeyValueStorage注入パターンでテスト可能。
- `lib/devtest.ts`:
  - 読み出しpre (`#devtest-readout`) は**既定で `display:none`**。起動時に
    localStorageの永続化選択のみ復元 (localStorage不在環境・破損値はOFF)。
  - `readoutToggle` アクション追加 (visibility反転 + localStorage永続化 + 即時反映)。
  - U行へ `ro=0|1` を追加 (`qp` と `note` の間。ドライバから可視状態を検証可能。
    追加位置は既存キーの前方一致検査 `U menu=1` 等を壊さない追加専用)。
- **reportCompact送出 (1秒周期の `devtest:` ログ行 + document.title) は
  表示とは独立に常時継続** — E2Eドライバの正本はログ側のため無影響。

## トグルキー

- **ctrl+shift+F12 (foCmdMode) → `m`**。
- PM指示の候補 `r` は foCmdModeで `foRetryConfirm` が既に使用中
  (wp7_e2e.shのリトライフローが依存) のため再割り当て不可。`m` は
  foCmdModeで空き、かつ全E2Eドライバ (wp7/8/9, qa_wp4) がfoCmdModeで
  `m` を送っていないことを確認済み。
- collCmdMode (ctrl+shift+F1) への同名バインドは意図的にしない
  (単一の記憶される割り当てとする。最小diff)。

## localStorage仕様

- キー `gento:devtest-readout`、値 `"1"`(可視) / `"0"`(非表示)、未設定=**OFF**。
- `gento:` 接頭辞のため resetStorage (ctrl+F1) で消去 → 再起動後は既定OFF。
  計測ドライバの初期状態決定論化 (スクリーンショットに読み出し写り込まない)
  を維持する意図的な設計。単体テストで契約pin済み。

## DevDrawer自動オープン判断 (現状維持 — PM指示どおり)

`App.svelte` の `devPanelOpen = enabled` (devビルドでDevDrawerが開く) は
**変更しない**。理由: (1) 今回の苦情は読み出しpre側 (position:fixed重ね合わせ)
であってDevDrawerはレイアウト内に押し込まれヘッダーを隠さない、
(2) ヘッダーのトグルボタンで明示的に閉じられる。devビルドの初見体験で
再苦情が出たら別WPで「既定閉・永続化」を検討する。

## テスト

- vitest `lib/devtest-readout.test.ts` (5 test、red→green実施):
  既定OFF (`null`/`"0"`/`""`/`"true"` → false)、`"1"`→true、トグル反転と
  永続化の往復、保存済みONの復元、`gento:` 接頭辞契約。
- **vitest全体 443 passed** (基準438 + 新規5) / cargo **690** (不変・Rust無変更)。
- lint・svelte-check 0 error (9 warningは全て既存の.svelte側)。

## E2E回帰・実機検証

- `bash docs/dev/measurements/drivers/wp8_e2e.sh`: **PASS=8 FAIL=0**
  (読み出し非表示でもドライバ成立。U行 `ro=0` を確認)。
- トグル実機検証 (Xephyr :5 + devtools releaseビルド、wp8と同じ手順):
  - 起動直後 `ro=0`、右上420x180の読み出し緑 (#7fe7a0) ピクセル **0**
  - ctrl+shift+F12 → m で `ro=1`、緑ピクセル **44** (表示復帰)
  - SIGTERM再起動後も `ro=1` (localStorage永続化)
  - 再トグルで `ro=0`、緑ピクセル **0**
- release-smoke (DISPLAY=:5): **全Step PASS**。featureなしバイナリの
  dev識別子不在・devtoolsビルドの計測シナリオとも無影響。

## 影響範囲の確認事項

- 画面上のreadoutを参照するドライバ・スクリプトは存在しない
  (repo内grep: devtest.ts自身のみ)。計測系はすべてログの `devtest:` 行を
  正本としているため、読み出し非表示化の影響を受けない。
- `document.title` チャネルも継続 (変更なし)。

## 教訓

- custom-protocol (release) ビルドはdistをバイナリ埋め込みするため、
  devtest.tsなどフロント変更後は `pnpm build` + cargo再ビルドが必須
  (既知の教訓の再確認。今回もE2E前に再ビルドして検証)。
