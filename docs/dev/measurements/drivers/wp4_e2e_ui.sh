#!/usr/bin/env bash
# WP-4 製品UIシェルの実機E2E検証 (devtoolsビルド + xdotool + Xディスプレイ)。
# 実行: bash docs/dev/measurements/drivers/wp4_e2e_ui.sh
#  (事前に cargo build --release -p gento-tauri --features custom-protocol,devtools)
#
# 注意: WMによる合成入力の座標変換に揺れがあるため、メニュー項目の直接
# クリックは「新規ウィンドウ出現 (OS連携アクション実行の証拠)」で判定する。
# 検証項目:
#   1) 起動時の自動スキャン (未登録フォルダをDB登録なしで閲覧, tot==N)
#   2) フォルダツリーのクリック → open_folder (Rustログ "folder opened")
#   3) Space → クイックプレビュー (中央の画相変化) / 矢印移動 / Esc → 復帰
#   4) 右クリック → コンテキストメニュー (devtest U行) → OS連携アクション実行
#   5) Enter → ビューア (devtest V行) → Esc
#   6) 再起動 → スクロール位置の復元 (devtest st)
set -euo pipefail

BIN=/mnt/holyland/Project/GEN-TO/target/release/gento-tauri
WORK=/tmp/opencode/wp4/e2e
DISPLAY="${DISPLAY:-:1}"
export NO_COLOR=1 GENTO_LOG=info GENTO_DEV_BRIDGE=1
export XDG_DATA_HOME="$WORK/env/data" XDG_CACHE_HOME="$WORK/env/cache"

rm -rf "$WORK"; mkdir -p "$WORK/env/data" "$WORK/env/cache" "$WORK/logs" "$WORK/shots"

python3 - "$WORK/ds" <<'PYEOF'
import struct, sys, zlib
from pathlib import Path
root = Path(sys.argv[1])
w = h = 64
def chunk(kind: bytes, data: bytes) -> bytes:
    return (struct.pack(">I", len(data)) + kind + data
            + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF))
for sub, count, tint in [("a", 40, 60), ("b", 4, 200)]:
    d = root / sub
    d.mkdir(parents=True, exist_ok=True)
    for i in range(count):
        raw = bytearray()
        for y in range(h):
            raw.append(0)
            for x in range(w):
                raw += bytes([(x * 3) % 256, tint, (i * 40) % 256, 255])
        png = (b"\x89PNG\r\n\x1a\n"
               + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
               + chunk(b"IDAT", zlib.compress(bytes(raw), 6))
               + chunk(b"IEND", b""))
        (d / f"img-{i:02}.png").write_bytes(png)
print("fixture ok")
PYEOF

pass_count=0
fail() { echo "[FAIL] $*" >&2; exit 1; }
pass() { pass_count=$((pass_count+1)); echo "[PASS] $*"; }

devtest_value() { grep -a "devtest:" "$WORK/logs/app.log" | tail -1 | grep -oE "\b$1=[-0-9.]+" | head -1 | cut -d= -f2; }
wait_log() { # <pattern> <timeout_s>
  local deadline=$((SECONDS + ${2:-30}))
  while (( SECONDS < deadline )); do
    grep -aqE -- "$1" "$WORK/logs/app.log" && return 0
    sleep 0.2
  done
  return 1
}

APP_PID=""; WID=""
launch() {
  : > "$WORK/logs/app.log"
  "$BIN" --dataset "$1" --cache-dir "$WORK/env/thumbs" >> "$WORK/logs/app.log" 2>&1 &
  APP_PID=$!
  for _ in $(seq 1 100); do
    WID=$(timeout 5 xdotool search --onlyvisible --pid "$APP_PID" 2>/dev/null | head -1 || true)
    [[ -n "$WID" ]] && break
    kill -0 "$APP_PID" 2>/dev/null || fail "app exited before window"
    sleep 0.2
  done
  [[ -n "$WID" ]] || fail "window not found"
  timeout 10 xdotool windowactivate --sync "$WID" >/dev/null 2>&1 || true
  sleep 2
}

terminate_app() {
  [[ -n "$APP_PID" ]] && kill -TERM "$APP_PID" 2>/dev/null || true
  for _ in $(seq 1 100); do kill -0 "$APP_PID" 2>/dev/null || break; sleep 0.2; done
  kill -9 "$APP_PID" 2>/dev/null || true
  wait "$APP_PID" 2>/dev/null || true
  APP_PID=""
}
trap terminate_app EXIT

# ウィンドウ相対座標 (CSS論理px→実スケール)。tauri既定window 1280x800。
scale=1; win_x=0; win_y=0
geo() {
  local out
  out=$(timeout 5 xdotool getwindowgeometry --shell "$WID")
  eval "$out"  # X Y WIDTH HEIGHT WINDOW
  scale=$(python3 -c "print($WIDTH/1280)")
}
sx() { python3 -c "print(int($1 * $scale + $X))"; }
sy() { python3 -c "print(int($1 * $scale + $Y))"; }
click_at() { timeout 10 xdotool mousemove "$1" "$2" click 1; }
rclick_at() { timeout 10 xdotool mousemove "$1" "$2" click 3; }

shot() { timeout 20 import -window "$WID" "$WORK/shots/$1.png" || true; }

center_lum() {
  python3 - "$1" <<'PYEOF'
import sys
from PIL import Image, ImageStat
img = Image.open(sys.argv[1]).convert("L")
w, h = img.size
box = img.crop((int(w*0.35), int(h*0.35), int(w*0.65), int(h*0.65)))
print(int(ImageStat.Stat(box).mean[0]))
PYEOF
}

echo "=== 1) 起動・自動スキャン ==="
launch "$WORK/ds/a"
wait_log "scan metrics" 30 || fail "auto scan did not run"
wait_log "devtest:" 20 || fail "dev bridge not flowing"
tot=$(devtest_value tot || true)
[[ "$tot" == "40" ]] || fail "expected tot=40, got ${tot:-none}"
pass "起動時の自動スキャン: 未登録フォルダをDB登録なしで閲覧 (tot=40)"
shot 01-initial

echo "=== 2) ツリーのホームクリック → open_folder ==="
geo
found=0
# UIUX-1: header下にメニューバー+左ペインタブストリップが追加された分
# (約+58px) ツリー先頭行の座標を更新。クリック先はツリー行 (タブボタン
# を押さないようY>=80に限定)。
for cy in 92 100 86 104 82; do
  click_at "$(sx 60)" "$(sy $cy)"
  sleep 1.2
  if grep -aq "folder opened (adhoc scan started)" "$WORK/logs/app.log"; then found=1; break; fi
done
[[ "$found" == "1" ]] || fail "tree click did not trigger open_folder"
pass "ツリーのノードクリック → open_folder (一時走査切替)"
shot 02-tree-clicked
# ホームの中身は画像が無い場合があるため、以降のステップはデータセットフォルダで
terminate_app
launch "$WORK/ds/a"
wait_log "devtest:" 40 || fail "devtest not flowing"
tot2=$(devtest_value tot || true)
[[ "$tot2" == "40" ]] || fail "relaunch did not rescan dataset (tot=$tot2)"

echo "=== 3) 選択 → Space クイックプレビュー ==="
xdotool key ctrl+F2; sleep 0.3
xdotool key F2; sleep 0.5
shot 03-before-space
lum_before=$(center_lum "$WORK/shots/03-before-space.png")
xdotool key space; sleep 1.5
shot 04-quick-preview
lum_qp=$(center_lum "$WORK/shots/04-quick-preview.png")
(( lum_qp < lum_before - 30 )) || fail "quick preview overlay not detected ($lum_before -> $lum_qp)"
pass "Space → クイックプレビュー (中央輝度 $lum_before → $lum_qp)"
xdotool key Right; sleep 1.0
shot 05-preview-next
xdotool key Escape; sleep 1.0
shot 06-preview-closed
lum_closed=$(center_lum "$WORK/shots/06-preview-closed.png")
(( lum_closed > lum_qp + 30 )) || fail "preview did not close ($lum_qp -> $lum_closed)"
pass "矢印移動 + Escでクイックプレビュー閉鎖 (輝度 $lum_qp → $lum_closed)"

echo "=== 4) 右クリック → コンテキストメニュー → OSのファイルマネージャーで表示 ==="
geo
shot 06b-grid
read CX CY < <(python3 - "$WORK/shots/06b-grid.png" <<'PYEOF2'
import sys
from PIL import Image
img = Image.open(sys.argv[1]).convert("RGB")
w, h = img.size
scale = w / 1280
best = None
for y in range(int(150*scale), min(int(700*scale), h-1), int(4*scale)):
    run = 0
    for x in range(int(280*scale), int(740*scale), int(4*scale)):
        r, g, b = img.getpixel((x, y))
        sat = max(r, g, b) - min(r, g, b)
        if sat > 50 and max(r, g, b) > 60:
            run += 1
            if run >= 4:
                best = (x - int(8*scale), y)
                break
        else:
            run = 0
    if best:
        break
print(*best) if best else print(0, 0)
PYEOF2
)
[[ "$CX" != "0" ]] || fail "no colorful thumbnail cell found"
RCX=$((X + CX)); RCY=$((Y + CY))
rclick_at "$RCX" "$RCY"
sleep 1.5
wait_log "U menu=1" 8 || fail "context menu did not open (U menu=0)"
shot 07-context-menu
pass "右クリック → コンテキストメニュー表示 (U menu=1, 3項目)"
# メニューを閉じてもう一度出し、先頭項目 (OSのファイルマネージャーで表示) を
# クリック位置の近傍 (メニュー内側上端) から実行 → ファイルマネージャ (nemo) の
# ウィンドウ出現でOS連携を証明する
xdotool key Escape; sleep 0.6
# 既存ウィンドウ集合を記録し、メニュー項目クリックで新規ウィンドウ (ファイル
# マネージャ/画像ビューア) が出現すればOS連携成功とみなす。入力座標の環境
# 揺れがあるため複数オフセットを試す。
before_windows=$(timeout 5 xdotool search --onlyvisible "" 2>/dev/null | sort | tr '\n' ',')
launched=""
for dx_dy in "24 68" "24 40" "40 100" "24 140"; do
  set -- $dx_dy
  DX=$1; DY=$2
  rclick_at "$RCX" "$RCY"
  sleep 1.3
  click_at "$((RCX + DX))" "$((RCY + DY))"
  for _ in $(seq 1 16); do
    now=$(timeout 5 xdotool search --onlyvisible "" 2>/dev/null | sort | tr '\n' ',')
    new=$(python3 -c "
b='${before_windows}'.split(',')
n='${now}'.split(',')
added=[w for w in n if w and w not in b]
print(' '.join(added))")
    if [[ -n "$new" ]]; then
      launched="$new"
      break
    fi
    sleep 0.5
  done
  if [[ -n "$launched" ]]; then break; fi
  xdotool key Escape 2>/dev/null || true; sleep 0.4
done
[[ -n "$launched" ]] || fail "context menu action did not launch any OS window"
for w in $launched; do xdotool windowclose "$w" 2>/dev/null || true; done
pass "コンテキストメニュー → OS連携アクション実行 (新規ウィンドウ: $launched)"
# パスをコピー (項目3) は座標注入の環境揺れが大きいため、ここではUI開示までを
# 単体試験 (clipboard.ts) と実機のメニュー表示で担保する

echo "=== 5) Enter → ビューア ==="
xdotool key ctrl+F2; sleep 0.3
xdotool key F2; sleep 0.3
xdotool key Return; sleep 2.0
shot 08-viewer
wait_log "V idx=" 10 && pass "Enter → ビューア表示 (devtest V行)" \
  || fail "viewer did not open"
xdotool key Escape; sleep 1.0

echo "=== 6) 再起動 → スクロール位置復元 ==="
# 今のフォルダ(ホーム)は行数が不明なため、データセットフォルダへ戻してから計測
terminate_app
launch "$WORK/ds/a"
wait_log "devtest:" 40 || fail "devtest not flowing"
xdotool key ctrl+F2; sleep 0.5
xdotool key Home; sleep 0.8
xdotool key --repeat 3 --delay 150 Page_Down; sleep 1.5
st_before=$(devtest_value st || true)
[[ "$st_before" =~ ^[0-9]+$ ]] && (( st_before > 0 )) || fail "scroll did not move (st=${st_before:-none})"
terminate_app
launch "$WORK/ds/a"
wait_log "devtest:" 40 || fail "devtest not flowing after restart"
sleep 4
st_after=$(devtest_value st || true)
[[ "$st_after" == "$st_before" ]] || fail "scroll not restored (before=$st_before after=$st_after)"
pass "再起動後のスクロール位置復元 (st=$st_before → $st_after)"
shot 09-restored
terminate_app

echo
echo "E2E RESULT: PASS ($pass_count checks)"
echo "shots: $WORK/shots"
