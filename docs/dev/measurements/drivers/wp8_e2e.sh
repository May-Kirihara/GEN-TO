#!/usr/bin/env bash
# WP-8 実機E2E検証 (devtoolsビルド + xdotool + Xディスプレイ)。
# 実行: bash docs/dev/measurements/drivers/wp8_e2e.sh
#  (事前に pnpm build (apps/desktop) および
#   cargo build --release -p gento-tauri --features custom-protocol,devtools)
#
# 検証項目 (WP-8受け入れ条件の実E2E証拠。8項目):
#   1) 登録ルート走査 (library G tot=6)
#   2) 日本語キーワード検索「青い髪」(trigram MATCH。tot=2)
#   3) 一致根拠の先読み (S ev=) → InfoPanel「検索一致」表示
#   4) 保存 (スマートコレクション) → 左ペインから表示して評価 (tot=2)
#   5a) 「sunset」検索 (tot=1)
#   5b) 検索結果ビューでInfoPanel評価編集 (★5付与。M rat=5)
#       — dev bridgeの評価クリックは ctrl+F4 = metaRate5
#         (ctrl+shift+F5 は focusCell1 なので注意。WP-8教訓)
#   5c) メタデータ編集後の条件付き再検索「sunset rating:>=5」(tot=1)
#   5d) 表示中検索の再実行 search_refresh (結果保持)
#
# 運用メモ (Xephyr前提):
#   共有デスクトップ (:1) が他セッションに占有されているとフォーカスが
#   不安定になるため、専用のネストXディスプレイ (:5 = Xephyr) で実行する
#   (wp7_e2e.shと同じ方針。Xephyrが無ければ起動する。WM無しでも
#   windowfocus + dev bridgeキーで駆動可)。明示指定は GENTO_E2E_DISPLAY。
#   DB検証はdev bridgeの機械可読行 (app.logの devtest: G/S/M行) で行う。
set -uo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
BIN=/mnt/holyland/Project/GEN-TO/target/release/gento-tauri
WORK=/tmp/opencode/wp8/e2e
DISPLAY="${GENTO_E2E_DISPLAY:-:5}"
export NO_COLOR=1 GENTO_LOG=info GENTO_DEV_BRIDGE=1
if ! xdpyinfo -display "$DISPLAY" >/dev/null 2>&1; then
  setsid nohup Xephyr "$DISPLAY" -screen 1600x1000 >/tmp/opencode/wp8-xephyr.log 2>&1 </dev/null &
  for _ in $(seq 1 50); do
    xdpyinfo -display "$DISPLAY" >/dev/null 2>&1 && break
    sleep 0.2
  done
fi
xdpyinfo -display "$DISPLAY" >/dev/null 2>&1 || { echo "no X display on $DISPLAY" >&2; exit 1; }

rm -rf "$WORK"; mkdir -p "$WORK/home" "$WORK/env/thumbs" "$WORK/env/db" "$WORK/dataset" "$WORK/shots"
SHOT="$WORK/shots"
NOTE="$WORK/e2e-notes.txt"; : > "$NOTE"
shot() { import -window root "$SHOT/$1.png" 2>/dev/null || true; }

python3 "$SCRIPT_DIR/wp8_mkdataset.py" "$WORK/dataset"

: > "$WORK/app.log"
HOME="$WORK/home" "$BIN" --dataset "$WORK/dataset" --cache-dir "$WORK/env/thumbs" \
  --db "$WORK/env/db/library.sqlite3" >> "$WORK/app.log" 2>&1 &
APP_PID=$!
cleanup() { kill -TERM "$APP_PID" 2>/dev/null; wait "$APP_PID" 2>/dev/null; }
trap cleanup EXIT
WID=""
for _ in $(seq 1 100); do
  WID=$(timeout 5 xdotool search --onlyvisible --pid "$APP_PID" 2>/dev/null | head -1 || true)
  [[ -n "$WID" ]] && break
  kill -0 "$APP_PID" 2>/dev/null || { echo "app died"; exit 1; }
  sleep 0.2
done
xdotool windowactivate --sync "$WID" >/dev/null 2>&1 || xdotool windowfocus "$WID"
sleep 4
echo "[step0] window up WID=$WID" | tee -a "$NOTE"

lastline() { grep -a "devtest:" "$WORK/app.log" | tail -1; }

# --- 1. datasetを登録ルートへ (foCmdMode: ctrl+shift+F12 → 0) ---
xdotool key ctrl+shift+F12; sleep 0.4; xdotool key 0
for _ in $(seq 1 90); do
  T=$(lastline | grep -oE "G tot=[0-9]+" | head -1 | cut -d= -f2)
  [[ "${T:-0}" == "6" ]] && break
  sleep 1
done
echo "[step1] library G tot=${T:-?} (expect 6)" | tee -a "$NOTE"
shot 01-library
[[ "${T:-0}" == "6" ]] && echo "PASS: library scan total=6" | tee -a "$NOTE"

sleep 12  # ハッシュ計算 (BLAKE3) の完了待ち (asset link→文書構築)
# --- 2. 日本語キーワード検索「青い髪」(filename一致・2件) ---
search() {  # $1=query
  xdotool key ctrl+shift+F12; sleep 0.4; xdotool key h; sleep 0.5
  xdotool type --delay 40 "$1"; sleep 0.3
  xdotool key Return; sleep 3
}
for _ in 1 2 3 4 5; do
  search "青い髪"
  if lastline | grep -qE 'S act=1 q="青い髪" tot=2'; then break; fi
  sleep 3
done
S=$(lastline)
echo "[step2] $S" | tee -a "$NOTE"
shot 02-search-result
grep -qE 'S act=1 q="青い髪" tot=2' <<<"$S" && echo "PASS: japanese search tot=2" | tee -a "$NOTE" \
  || echo "FAIL: japanese search ($S)" | tee -a "$NOTE"

# --- 3. 一致根拠: focusCell0 → evidence先読み ---
xdotool key ctrl+shift+F4; sleep 2.5
S=$(lastline)
echo "[step3] $S" | tee -a "$NOTE"
shot 03-evidence
grep -qE "ev=[1-9]" <<<"$S" && echo "PASS: evidence loaded" | tee -a "$NOTE" \
  || echo "FAIL: evidence ($S)" | tee -a "$NOTE"

# --- 4. 保存 (u=srSave 固定名) → スマートコレクション反映 (coll 3) ---
xdotool key ctrl+shift+F12; sleep 0.4; xdotool key u; sleep 2
shot 04-saved
xdotool key ctrl+shift+F1; sleep 0.4; xdotool key 3; sleep 2.5
S=$(lastline)
echo "[step4] smart collection show: $S" | tee -a "$NOTE"
shot 05-smart-collection
grep -qE "G tot=2 " <<<"$S" && echo "PASS: saved search (=smart collection) evaluates tot=2" | tee -a "$NOTE" \
  || echo "FAIL: smart collection ($S)" | tee -a "$NOTE"

# --- 5. メタデータ編集→結果更新 ---
# 5a: sunsetを1件ヒットさせる
search "sunset"
S=$(lastline)
echo "[step5a] $S" | tee -a "$NOTE"
grep -qE 'q="sunset" tot=1' <<<"$S" && echo "PASS: sunset tot=1" | tee -a "$NOTE" \
  || echo "FAIL: sunset ($S)" | tee -a "$NOTE"
# 5b: 先頭選択 (ctrl+shift+F4=focusCell0) → 評価5 (ctrl+F4=metaRate5)。
#     ※ ctrl+shift+F5 は focusCell1 (WP-5のF2..F8ブロック) なので使わない。
xdotool key ctrl+shift+F4; sleep 2.5
xdotool key ctrl+F4; sleep 3
M=$(lastline)
echo "[step5b] rated: $M" | tee -a "$NOTE"
shot 06-rated
grep -qE "rat=5|frat=5" <<<"$M" && echo "PASS: rating=5 applied" | tee -a "$NOTE" \
  || echo "FAIL: rating ($M)" | tee -a "$NOTE"
# 5c: 条件付き再検索 (メタデータ編集で結果が更新)
search "sunset rating:>=5"
S=$(lastline)
echo "[step5c] $S" | tee -a "$NOTE"
shot 07-updated-results
grep -qE 'q="sunset rating:>=5" tot=1' <<<"$S" && echo "PASS: metadata edit updates results" | tee -a "$NOTE" \
  || echo "FAIL: after edit ($S)" | tee -a "$NOTE"
# 5d: 表示中検索の再実行 (search_refresh: l)
xdotool key ctrl+shift+F12; sleep 0.4; xdotool key l; sleep 2.5
S=$(lastline)
echo "[step5d] refresh: $S" | tee -a "$NOTE"
shot 08-refresh
grep -qE 'q="sunset rating:>=5" tot=1' <<<"$S" && echo "PASS: search_refresh keeps results" | tee -a "$NOTE" \
  || echo "FAIL: refresh ($S)" | tee -a "$NOTE"

echo "=== E2E summary ===" | tee -a "$NOTE"
echo "PASS=$(grep -c '^PASS' "$NOTE") FAIL=$(grep -c '^FAIL' "$NOTE")" | tee -a "$NOTE"
[[ "$(grep -c '^FAIL' "$NOTE")" == "0" ]]
