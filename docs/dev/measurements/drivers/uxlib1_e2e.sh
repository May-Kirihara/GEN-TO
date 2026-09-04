#!/usr/bin/env bash
# UXLIB-1 実機E2E検証 (devtoolsビルド + xdotool + Xディスプレイ)。
# 実行: bash docs/dev/measurements/drivers/uxlib1_e2e.sh
#  (事前に pnpm build (apps/desktop) および
#   cargo build --release -p gento-tauri --features custom-protocol,devtools)
#
# 検証項目 (UXLIB-1受け入れ条件の実E2E証拠):
#   1) --dataset 起動 (未登録) → adhoc表示 + InfoPanel理由 (lock=u)
#   2) LibraryPanel入力に実キー入力でパス→追加 → library表示へ自動切替
#      → 画像表示 (ピクセル検証 — FIXLIB-1で統計値totのみの偽陽性を封じ)
#      → (ハッシュ後) 編集可 (★5付与で実証)
#   3) 再起動 (--dataset同じ=登録済み) → library表示で起動 + 画像表示 (ピクセル)
#   4) --dataset無し再起動 (CWDにsample/dataset無し) → library表示で起動
#   5) ツリーの登録外フォルダ (home/AAA-unreg) → adhoc + 理由文言 (lock=u)
#   5b) 「ライブラリを表示」ボタン → library表示へ切替
#   5c) 登録ルート行クリック → library表示へ切替
#   6) SIGTERM graceful終了 (releaseスモーク要素)
#
# ピクセル検証 (FIXLIB-1): 「画像表示」はdevtest統計 (tot/rdy) ではなく
#   (a) devtest IM行 ok>=N (セル<img>がデコード完了 — DOM実測)
#   (b) スクリーンショットのグリッド領域 unique colors >= 閾値 (描画実測)
#   の両方で判定する。プレースホルダーのグリッドは数十色程度、
#   描画済みグリッドは数千色以上になる (FIXLIB-1修正前: 609色/修正後: 26,000色
#   — sample/dataset 1615件実測。4枚fixtureでは閾値を下げて運用)。
#
# 運用メモ: wp8/w9と同じ方針 (専用Xディスプレイ :5 = Xephyr。無ければ起動)。
# DB検証はdev bridgeの機械可読行 (app.logの devtest: G/M行) で行う。
set -uo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
BIN=/mnt/holyland/Project/GEN-TO/target/release/gento-tauri
WORK=/tmp/opencode/uxlib1/e2e
# Xephyrのホスト表示は起動時のDISPLAY (上書き前に退避)
HOST_DISPLAY="${DISPLAY:-:0}"
DISPLAY="${GENTO_E2E_DISPLAY:-:5}"
export NO_COLOR=1 GENTO_LOG=info GENTO_DEV_BRIDGE=1
# グリッド領域ピクセル判定の閾値 (描画済み>=此値、プレースホルダー<<此値)
PIXEL_MIN_COLORS=800
if ! xdpyinfo -display "$DISPLAY" >/dev/null 2>&1; then
  env DISPLAY="$HOST_DISPLAY" setsid nohup Xephyr "$DISPLAY" -screen 1600x1000 >/tmp/opencode/uxlib1-xephyr.log 2>&1 </dev/null &
  for _ in $(seq 1 50); do
    xdpyinfo -display "$DISPLAY" >/dev/null 2>&1 && break
    sleep 0.2
  done
fi
xdpyinfo -display "$DISPLAY" >/dev/null 2>&1 || { echo "no X display on $DISPLAY" >&2; exit 1; }

rm -rf "$WORK"; mkdir -p "$WORK/home" "$WORK/env/thumbs" "$WORK/env/db" "$WORK/dataset" \
  "$WORK/home/AAA-unreg" "$WORK/appcwd" "$WORK/shots" "$WORK/logs"
SHOT="$WORK/shots"
NOTE="$WORK/e2e-notes.txt"; : > "$NOTE"
shot() { import -window root "$SHOT/$1.png" 2>/dev/null || true; }
pass() { echo "PASS: $*" | tee -a "$NOTE"; }
fail() { echo "FAIL: $*" | tee -a "$NOTE"; FAILURES=$((FAILURES + 1)); }
FAILURES=0

devtest_value() {
  sed 's/\x1b\[[0-9;]*m//g' "$WORK/logs/app.log" 2>/dev/null | grep -a "devtest:" | tail -1 \
    | grep -oE "\b$1=[-0-9A-Za-z_.]*" | head -1 | cut -d= -f2
}
wait_value() { # <key> <expected> <timeout_s>
  local deadline=$((SECONDS + ${3:-30})) v=""
  while (( SECONDS < deadline )); do
    v=$(devtest_value "$1" || true)
    [[ "$v" == "$2" ]] && return 0
    sleep 0.3
  done
  return 1
}
wait_value_ge() { # <key> <min> <timeout_s> — devtest行の数値keyがmin以上になるまで
  local deadline=$((SECONDS + ${3:-30})) v=""
  while (( SECONDS < deadline )); do
    v=$(devtest_value "$1" || true)
    [[ "$v" =~ ^[0-9]+$ ]] && (( v >= $2 )) && return 0
    sleep 0.3
  done
  return 1
}
wait_log() { # <pattern> <timeout_s>
  local deadline=$((SECONDS + ${2:-30}))
  while (( SECONDS < deadline )); do
    grep -aqE -- "$1" "$WORK/logs/app.log" && return 0
    sleep 0.2
  done
  return 1
}
# グリッド中央領域のunique color数 (FIXLIB-1ピクセル判定)。
grid_pixel_colors() { # <png> → 標準出力へ数値 (失敗時0)
  python3 - "$1" <<'PYEOF' 2>/dev/null || echo 0
import sys
from PIL import Image
img = Image.open(sys.argv[1]).convert("RGB")
w, h = img.size
crop = img.crop((int(w * 0.35), int(h * 0.25), int(w * 0.70), int(h * 0.85)))
colors = crop.getcolors(maxcolors=1_000_000)
print(len(colors) if colors else 1_000_000)
PYEOF
}

# fixture: dataset 4枚 + 登録外フォルダ 1枚 (実PNG)。
# ピクセル判定のため画素ごとに変化するグラデーション (1枚あたり数百色以上)。
python3 - "$WORK/dataset" "$WORK/home/AAA-unreg" <<'PYEOF'
import struct, sys, zlib
from pathlib import Path
def chunk(kind: bytes, data: bytes) -> bytes:
    return (struct.pack(">I", len(data)) + kind + data
            + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF))
def png(path: Path, seed: int) -> None:
    w = h = 32
    def px(x: int, y: int) -> bytes:
        return bytes(((x * 8 + seed * 30) % 256, (y * 8 + seed * 17) % 256,
                      ((x * 3 + y * 5 + seed * 40) % 256), 255))
    raw = b"".join(b"\x00" + b"".join(px(x, y) for x in range(w)) for y in range(h))
    path.write_bytes(b"\x89PNG\r\n\x1a\n"
                     + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
                     + chunk(b"IDAT", zlib.compress(raw, 6)) + chunk(b"IEND", b""))
dataset, unreg = (Path(a) for a in sys.argv[1:3])
for i in range(4):
    png(dataset / f"ux-{i:02}.png", i)
png(unreg / "unreg.png", 9)
print("fixture: dataset 4 pngs + AAA-unreg 1 png")
PYEOF

APP_PID=""
launch() { # $1 = "dataset" | "nodataset"
  : > "$WORK/logs/app.log"
  if [[ "$1" == "dataset" ]]; then
    HOME="$WORK/home" "$BIN" --dataset "$WORK/dataset" \
      --cache-dir "$WORK/env/thumbs" --db "$WORK/env/db/library.sqlite3" \
      >> "$WORK/logs/app.log" 2>&1 &
  else
    # --dataset無し: CWD依存の既定 (sample/dataset) が不在になるよう隔離CWDから起動
    ( cd "$WORK/appcwd" && exec env HOME="$WORK/home" "$BIN" \
        --cache-dir "$WORK/env/thumbs" --db "$WORK/env/db/library.sqlite3" ) \
      >> "$WORK/logs/app.log" 2>&1 &
  fi
  APP_PID=$!
  local WID=""
  for _ in $(seq 1 100); do
    WID=$(timeout 5 xdotool search --onlyvisible --pid "$APP_PID" 2>/dev/null | head -1 || true)
    [[ -n "$WID" ]] && break
    kill -0 "$APP_PID" 2>/dev/null || { echo "app exited before window" | tee -a "$NOTE"; exit 1; }
    sleep 0.2
  done
  [[ -n "$WID" ]] || { echo "window not found" | tee -a "$NOTE"; exit 1; }
  timeout 10 xdotool windowactivate --sync "$WID" >/dev/null 2>&1 || xdotool windowfocus "$WID"
  sleep 3
}
graceful_stop() { # SIGTERMで落ちること (kill -9不使用) を検証
  [[ -n "$APP_PID" ]] || return 0
  kill -TERM "$APP_PID" 2>/dev/null || true
  local deadline=$((SECONDS + 15))
  while (( SECONDS < deadline )); do
    kill -0 "$APP_PID" 2>/dev/null || { wait "$APP_PID" 2>/dev/null; APP_PID=""; return 0; }
    sleep 0.2
  done
  kill -9 "$APP_PID" 2>/dev/null || true
  wait "$APP_PID" 2>/dev/null || true
  APP_PID=""
  return 1
}
trap '[[ -n "$APP_PID" ]] && kill -9 "$APP_PID" 2>/dev/null; true' EXIT

# ========== 1. --dataset起動 (未登録) → adhoc + 理由 ==========
launch dataset
wait_log "devtest:" 30 || fail "dev bridge not flowing"
if wait_value mode real 20; then
  pass "1) --dataset (未登録) 起動はadhoc一時走査 (mode=real)"
else
  fail "1) 未登録--dataset起動がadhocでない (mode=$(devtest_value mode || echo none))"
fi
wait_value tot 4 20 || fail "1) adhocカタログ4件 (tot=$(devtest_value tot || echo none))"
xdotool key ctrl+shift+F4; sleep 1.5   # focusCell0
if wait_value edit 0 10 && [[ "$(devtest_value lock || true)" == "u" ]]; then
  pass "1) adhoc表示は編集不可 + 理由「未登録フォルダ」 (edit=0 lock=u)"
else
  fail "1) 理由表示不全 (edit=$(devtest_value edit || echo -) lock=$(devtest_value lock || echo -))"
fi
shot 01-adhoc-unregistered

# ========== 2. LibraryPanelへ実キー入力でパス→追加 → library自動切替 ==========
xdotool key ctrl+shift+F2; sleep 0.4   # 登録ルートパス入力へフォーカス
xdotool type --delay 25 "$WORK/dataset"; sleep 0.3
xdotool key Return; sleep 1.5
if wait_value mode library 20; then
  pass "2) addRoot成功 → library表示へ自動切替 (mode=library)"
else
  fail "2) 自動切替されない (mode=$(devtest_value mode || echo none))"
fi
wait_value tot 4 20 || fail "2) library表示で登録画像が見える (tot=$(devtest_value tot || echo none))"
shot 02-after-addroot-autoswitch
# ハッシュ完了 (BLAKE3) 待ち → 編集可を実証 (★5付与)
xdotool key ctrl+shift+F4; sleep 1.0
wait_value fdb asset 40 || fail "2) ハッシュ完了待ち (fdb=$(devtest_value fdb || echo -))"
if wait_value edit 1 10; then
  pass "2) (ハッシュ後) 編集可 (edit=1)"
else
  fail "2) 編集可にならない (edit=$(devtest_value edit || echo -))"
fi
xdotool key ctrl+F4; sleep 2.0         # metaRate5
wait_value rat 5 20 && pass "2) ★5付与に成功 (rat=5 — 編集の実証)" \
  || fail "2) ★5付与失敗 (rat=$(devtest_value rat || echo -))"
# 画像表示の実測 (FIXLIB-1): DOM (IM ok>=4) + ピクセル (グリッド領域色数)
if wait_value_ge ok 4 20; then
  pass "2) グリッド<img>デコード実測 (IM ok=4)"
else
  fail "2) <img>がデコードされていない (IM ok=$(devtest_value ok || echo -))"
fi
shot 03-editable-after-hash
shot_colors=$(grid_pixel_colors "$SHOT/03-editable-after-hash.png")
if (( shot_colors >= PIXEL_MIN_COLORS )); then
  pass "2) library表示のピクセル描画実測 (unique colors=${shot_colors} >= ${PIXEL_MIN_COLORS})"
else
  fail "2) グリッドが描画されていない可能性 (unique colors=${shot_colors} < ${PIXEL_MIN_COLORS})"
fi

# ========== 3. 再起動 (--dataset同じ=登録済み) → library起動 ==========
if graceful_stop; then
  pass "6) SIGTERM graceful終了 (①回目)"
else
  fail "6) SIGTERMで終了しない"
fi
launch dataset
wait_log "devtest:" 30 || fail "dev bridge not flowing after restart"
if wait_value mode library 30; then
  pass "3) 登録済み--dataset再起動はlibrary表示で起動 (mode=library)"
else
  fail "3) 再起動がlibraryでない (mode=$(devtest_value mode || echo none))"
fi
wait_value tot 4 20 || fail "3) library表示4件 (tot=$(devtest_value tot || echo none))"
if wait_value_ge ok 4 20; then
  pass "3) 再起動library表示の<img>デコード実測 (IM ok=4)"
else
  fail "3) 再起動後<img>がデコードされていない (IM ok=$(devtest_value ok || echo -))"
fi
shot 04-restart-registered-library
restart_colors=$(grid_pixel_colors "$SHOT/04-restart-registered-library.png")
if (( restart_colors >= PIXEL_MIN_COLORS )); then
  pass "3) 再起動library表示のピクセル描画実測 (unique colors=${restart_colors} >= ${PIXEL_MIN_COLORS})"
else
  fail "3) 再起動後グリッドが描画されていない可能性 (unique colors=${restart_colors} < ${PIXEL_MIN_COLORS})"
fi

# ========== 4. --dataset無し再起動 → library起動 ==========
if graceful_stop; then pass "6) SIGTERM graceful終了 (②回目)"; else fail "6) SIGTERMで終了しない"; fi
launch nodataset
wait_log "devtest:" 30 || fail "dev bridge not flowing after restart (nodataset)"
if wait_value mode library 30; then
  pass "4) --dataset無し再起動もlibrary表示で起動 (mode=library)"
else
  fail "4) --dataset無し起動がlibraryでない (mode=$(devtest_value mode || echo none))"
fi
wait_value tot 4 20 || fail "4) library表示4件 (tot=$(devtest_value tot || echo none))"
shot 05-restart-nodataset-library

# ========== 5. ツリーの登録外フォルダ → adhoc + 理由 ==========
xdotool key shift+F7; sleep 2.5        # openFirstHomeChild → home/AAA-unreg
if wait_value mode real 20; then
  pass "5) 登録外フォルダ選択はadhocへ切替 (mode=real)"
else
  fail "5) 登録外フォルダでadhocにならない (mode=$(devtest_value mode || echo none))"
fi
wait_value tot 1 20 || fail "5) 登録外フォルダ1件 (tot=$(devtest_value tot || echo none))"
xdotool key ctrl+shift+F4; sleep 1.5
if wait_value edit 0 10 && [[ "$(devtest_value lock || true)" == "u" ]]; then
  pass "5) adhoc + 理由「未登録フォルダ——ルート登録すると編集できます」 (edit=0 lock=u)"
else
  fail "5) 理由表示不全 (edit=$(devtest_value edit || echo -) lock=$(devtest_value lock || echo -))"
fi
shot 06-adhoc-reason

# 5b. 「ライブラリを表示」ボタン → library切替
xdotool key ctrl+shift+F12; sleep 0.4; xdotool key 2; sleep 2
if wait_value mode library 20; then
  pass "5b) 「ライブラリを表示」ボタンでlibraryへ切替 (mode=library)"
else
  fail "5b) ボタンで切替されない (mode=$(devtest_value mode || echo none))"
fi
shot 07-show-library-button

# 5c. 登録ルート行クリック → library切替 (adhocへ戻してから)
xdotool key shift+F7; sleep 2.5
wait_value mode real 20 || fail "5c) 準備: 登録外フォルダへ戻れない (mode=$(devtest_value mode || echo none))"
xdotool key ctrl+shift+F12; sleep 0.4; xdotool key 1; sleep 2
if wait_value mode library 20; then
  pass "5c) 登録ルート行クリックでlibraryへ切替 (mode=library)"
else
  fail "5c) 行クリックで切替されない (mode=$(devtest_value mode || echo none))"
fi
shot 08-root-row-click

# ========== 6. 終了 (graceful) ==========
if graceful_stop; then pass "6) SIGTERM graceful終了 (③回目 — releaseスモーク)"; else fail "6) SIGTERMで終了しない"; fi

echo | tee -a "$NOTE"
echo "UXLIB1 E2E RESULT: $((18 - FAILURES))/18 conditions passed, FAILURES=$FAILURES" | tee -a "$NOTE"
if (( FAILURES > 0 )); then
  echo "FAILING LINES:" | tee -a "$NOTE"
  grep -a "devtest:" "$WORK/logs/app.log" | tail -5 | tee -a "$NOTE"
  exit 1
fi
exit 0
