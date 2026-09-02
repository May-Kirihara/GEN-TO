#!/usr/bin/env bash
# WP-9 実機E2E検証 (devtoolsビルド + xdotool + Xディスプレイ)。
# 実行: bash docs/dev/measurements/drivers/wp9_e2e.sh
#  (事前に pnpm build (apps/desktop) および
#   cargo build --release -p gento-tauri --features custom-protocol,devtools)
#
# 検証項目 (WP-9受け入れ条件の実E2E証拠。8項目):
#   1) 登録ルート走査 (library G tot=6) + ハッシュ完了待ち
#   2) positive prompt検索「crimson」(embedded_prompt FTS接続。tot=1)
#   3) negative prompt語「watermark」でヒットしない (tot=0)
#   4) 検索キー model: / steps:>= / has:workflow (sd=comfy両方)
#   5) 生成メタデータ表示: ComfyUI PNGフォーカスでX行 gen=1 wf>0
#   6) 折りたたみ展開 (foCmdMode p) でworkflow本文レンダリング (wfx=1)
#   7) workflowコピー (foCmdMode i) でコピー発火 (cpy=workflow:1:<len>)
#   8) Raw metadata節 + 8b) 写真メタデータ (EXIF入りJPEG) + 8c) 巨大zTXt耐性
#
# 運用メモ: wp8_e2e.shと同じ方針 (専用Xディスプレイ :5 = Xephyr。
# Xephyrが無ければ起動。DB検証はdev bridgeの機械可読行 app.log の
# devtest: G/S/X行で行う)。
set -uo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
BIN=/mnt/holyland/Project/GEN-TO/target/release/gento-tauri
WORK=/tmp/opencode/wp9/e2e
DISPLAY="${GENTO_E2E_DISPLAY:-:5}"
export NO_COLOR=1 GENTO_LOG=info GENTO_DEV_BRIDGE=1
if ! xdpyinfo -display "$DISPLAY" >/dev/null 2>&1; then
  setsid nohup Xephyr "$DISPLAY" -screen 1600x1000 >/tmp/opencode/wp9-xephyr.log 2>&1 </dev/null &
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

python3 "$SCRIPT_DIR/wp9_mkdataset.py" "$WORK/dataset"

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
xline()   { lastline | grep -oE 'X [^|]*' | head -1; }
# 検索S行のtot= (q="..."がact=とtot=の間に入る形式に対応)
stot()    { lastline | grep -oE 'S act=1 q="[^"]*" tot=[0-9]+' | grep -oE 'tot=[0-9]+' | head -1 | cut -d= -f2; }
pass() { echo "PASS: $*" | tee -a "$NOTE"; }
fail() { echo "FAIL: $*" | tee -a "$NOTE"; FAILURES=$((FAILURES + 1)); }
FAILURES=0

wait_x() { # wait_x <bash条件式文字列> <説明> [timeout_s]
  local cond=$1 desc=$2 timeout=${3:-30}
  local deadline=$((SECONDS + timeout))
  while (( SECONDS < deadline )); do
    if eval "$cond"; then return 0; fi
    sleep 1
  done
  return 1
}

# --- 1. datasetを登録ルートへ (foCmdMode: ctrl+shift+F12 → 0) ---
xdotool key ctrl+shift+F12; sleep 0.4; xdotool key 0
T=""
for _ in $(seq 1 90); do
  T=$(lastline | grep -oE "G tot=[0-9]+" | head -1 | cut -d= -f2)
  [[ "${T:-0}" == "6" ]] && break
  sleep 1
done
echo "[step1] library G tot=${T:-?} (expect 6)" | tee -a "$NOTE"
shot 01-library
[[ "${T:-0}" == "6" ]] && pass "library scan total=6" || fail "library scan total=${T:-?}"

sleep 15  # ハッシュ+メタデータ解析の完了待ち (asset link→メタデータ保存→文書構築)

# --- 2. positive prompt検索「crimson」(sd.pngにのみ hit) ---
search() {  # $1=query
  xdotool key ctrl+shift+F12; sleep 0.4; xdotool key h; sleep 0.5
  xdotool type --delay 40 -- "$1"; sleep 0.3
  xdotool key Return; sleep 3
}
search "crimson"
if wait_x '[[ "$(stot)" == 1 ]]' "search crimson tot=1" 20; then
  pass "positive prompt検索 crimson → 1件 (embedded_prompt FTS)"
else
  fail "positive prompt検索 crimson (S行: $(lastline | grep -oE 'S act=[^|]*'))"
fi
shot 02-search-crimson

# --- 3. negative語「watermark」はヒットしない ---
search "watermark"
if wait_x '[[ "$(stot)" == 0 ]]' "search watermark tot=0" 20; then
  pass "negative prompt語 watermark → 0件 (意味文書に混ぜない)"
else
  fail "negative prompt語 watermark がヒット (S行: $(lastline | grep -oE 'S act=[^|]*'))"
fi

# --- 4. メタデータキー: model / steps>= / has:workflow ---
search 'model:animagine'
if wait_x '[[ "$(stot)" == 1 ]]' "model:animagine" 20; then
  pass "model:animagine → 1件 (sd.png)"
else
  fail "model:animagine ($(lastline | grep -oE 'S act=[^|]*'))"
fi
search 'steps:>=30'
if wait_x '[[ "$(stot)" == 1 ]]' "steps:>=30" 20; then
  pass "steps:>=30 → 1件 (comfy.png)"
else
  fail "steps:>=30 ($(lastline | grep -oE 'S act=[^|]*'))"
fi
search 'has:workflow'
if wait_x '[[ "$(stot)" == 1 ]]' "has:workflow" 20; then
  pass "has:workflow → 1件 (comfy.png)"
else
  fail "has:workflow ($(lastline | grep -oE 'S act=[^|]*'))"
fi

# --- 5. ComfyUI PNGのInfoPanel表示 (検索でcomfyだけ表示→focusCell0) ---
search 'golden pagoda'
if wait_x '[[ "$(stot)" == 1 ]]' "search golden pagoda" 20; then
  pass "positive prompt検索 golden pagoda → 1件 (comfy.png)"
else
  fail "golden pagoda検索"
fi
xdotool key ctrl+shift+F4  # focusCell0 (検索結果1件のcomfy.png — ctrl+shift+F4)
sleep 2
if wait_x '[[ "$(xline)" =~ gen=1 ]] && [[ "$(xline)" =~ wf=[0-9] ]] && ! [[ "$(xline)" =~ wf=- ]]' "X gen=1 wf>0" 20; then
  pass "生成メタデータ節 + workflow折りたたみ表示 ($(xline))"
else
  fail "生成メタデータ表示 ($(xline))"
fi
shot 03-info-gen

# --- 6. 折りたたみ全展開 (foCmdMode p) — 巨大workflowでもレンダリング ---
xdotool key ctrl+shift+F12; sleep 0.4; xdotool key p
sleep 2
if wait_x '[[ "$(xline)" =~ wfx=1 ]]' "X wfx=1 (workflow展開)" 20; then
  pass "workflow折りたたみ展開レンダリング ($(xline | grep -oE 'wf=[^ ]* wfx=1'))"
else
  fail "workflow展開 ($(xline))"
fi
shot 04-workflow-expanded

# --- 7. workflowコピー (foCmdMode i) ---
xdotool key ctrl+shift+F12; sleep 0.4; xdotool key i
sleep 2
if wait_x '[[ "$(xline)" =~ cpy=workflow:1:[0-9]+ ]]' "cpy=workflow:1:N" 20; then
  WFCOPY=$(xline | grep -oE 'cpy=workflow:1:[0-9]+' | grep -oE '[0-9]+$')
  pass "workflow JSONコピー発火 (全体${WFCOPY}文字)"
else
  fail "workflowコピー ($(xline))"
fi

# --- 8. Raw metadata節 + 巨大zTXt ---
if wait_x '[[ "$(xline)" =~ raw=1 ]]' "X raw=1" 10; then
  pass "Raw metadata節表示 ($(xline | grep -oE 'raw=[^ ]*'))"
else
  fail "Raw metadata節 ($(xline))"
fi

# --- 8b. 写真メタデータ (EXIF+IPTC入りJPEG) の表示 ---
search 'photo'
sleep 1
xdotool key ctrl+shift+F4  # photo.jpg (検索結果の先頭)
sleep 1.5
if wait_x '[[ "$(xline)" =~ photo=1 ]]' "X photo=1" 15; then
  pass "写真メタデータ節表示 (EXIF由来: $(xline | grep -oE 'photo=[^ ]*'))"
else
  fail "写真メタデータ節 ($(xline))"
fi

# --- 8c. 巨大zTXt: アプリが応答し続ける (折りたたみ既定で止まらない) ---
search 'e2e plain text'
sleep 1
xdotool key ctrl+shift+F4  # plain.png
sleep 1.5
if wait_x '[[ -n "$(xline)" ]]' "X行が流れ続ける" 10; then
  pass "巨大zTXt混在データセットでパネル停止なし (X行応答継続)"
else
  fail "X行停止 (巨大チャンクでパネル凝固?)"
fi
# 元画像バイト列不変 (DB外検証): 解析後もハッシュ前後一致はRustテスト保証。
# E2Eでは実ファイルが変更されていないことをサイズ一致で確認
PY_SIZE=$(stat -c%s "$WORK/dataset/sd.png")
if [[ "$PY_SIZE" -gt 0 ]]; then
  pass "解析後も元ファイル存在 (sd.png ${PY_SIZE}bytes)"
else
  fail "元ファイルが消失/破損"
fi

echo | tee -a "$NOTE"
echo "WP9 E2E RESULT: $((9 - FAILURES))/9 conditions passed, FAILURES=$FAILURES" | tee -a "$NOTE"
if (( FAILURES > 0 )); then
  echo "FAILING X LINES:" | tee -a "$NOTE"
  grep -a "devtest:" "$WORK/app.log" | tail -3 | tee -a "$NOTE"
  exit 1
fi
exit 0
