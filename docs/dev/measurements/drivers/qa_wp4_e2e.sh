#!/usr/bin/env bash
# QA-WP4 実機E2E検証 (devtoolsビルド + xdotool + Xディスプレイ)。
# 実行: bash docs/dev/measurements/drivers/qa_wp4_e2e.sh
#  (事前に pnpm build (apps/desktop) および
#   cargo build --release -p gento-tauri --features custom-protocol,devtools)
#
# 検証項目 (QA-WP4受け入れ「実E2E」の証拠採取):
#   1) 走査中のフォルダ切替 (Major-1): ファイルシステム(/)走査中 (遅い。
#      深さ16ガードが効く本物の全FS走査) にツリーでB (30件) をクリック
#      → 切替が成功し (旧Err "cannot switch folder while scanning" は廃止)、
#        旧走査は "scan abandoned (superseded by newer scan)" で破棄、
#        最終一覧がBのみ (tot==30。/ の結果は混入しない)
#   1b) 走査キャンセル (Major-1): / 走査中にキャンセルボタン →
#      "adhoc scan cancelled" で停止 (完了扱いにならない)
#   2) adhocセッション再開 (Major-2): mtime降順ソートでセッション位置を進め
#      → 再起動 (name順に戻る) → 再開 → 保存時のフォルダ+ソートが復元され
#      同じ画像 (同一AssetId・同一表示名) にフォーカス
#   2b) 件数不一致警告 (Major-2): ファイル削除 (>5%ずれ) 後の再開は
#      警告表示 + 再開中止 (act=0)
#
# 入力駆動について: 本環境はxdotoolポインタ (mousemove/click) の到達が
# 不安定なため (devtest.tsの設計由来)、ツリー・ボタン操作はdev bridgeの
# F-keyアクション (本物のDOM click → 生産ハンドラ) で駆動する。
set -euo pipefail

BIN=/mnt/holyland/Project/GEN-TO/target/release/gento-tauri
WORK=/tmp/opencode/qa_wp4/e2e
DISPLAY="${DISPLAY:-:1}"
export NO_COLOR=1 GENTO_LOG=info GENTO_DEV_BRIDGE=1

rm -rf "$WORK"; mkdir -p "$WORK/env/data" "$WORK/env/cache" "$WORK/logs" "$WORK/shots" "$WORK/home"

pass_count=0
fail() { echo "[FAIL] $*" >&2; exit 1; }
pass() { pass_count=$((pass_count+1)); echo "[PASS] $*"; }

devtest_value() { grep -a "devtest:" "$WORK/logs/app.log" | tail -1 | grep -oE "\b$1=[-0-9A-Za-z_.]*" | head -1 | cut -d= -f2; }
wait_log() { # <pattern> <timeout_s>
  local deadline=$((SECONDS + ${2:-30}))
  while (( SECONDS < deadline )); do
    grep -aqE -- "$1" "$WORK/logs/app.log" && return 0
    sleep 0.2
  done
  return 1
}
count_log() { grep -aE -- "$1" "$WORK/logs/app.log" | wc -l || true; }

APP_PID=""; WID=""
launch() { # launch <dataset> <home>
  : > "$WORK/logs/app.log"
  HOME="$2" "$BIN" --dataset "$1" --cache-dir "$WORK/env/thumbs" \
    >> "$WORK/logs/app.log" 2>&1 &
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

shot() { timeout 20 import -window "$WID" "$WORK/shots/$1.png" || true; }

# --- フィクスチャ -----------------------------------------------------------
# B: 切替先の小さな実PNGフォルダ (30件)
python3 - "$WORK/home/B" <<'PYEOF'
import struct, sys, zlib
from pathlib import Path
root = Path(sys.argv[1]); root.mkdir(parents=True, exist_ok=True)
w = h = 32
def chunk(kind: bytes, data: bytes) -> bytes:
    return (struct.pack(">I", len(data)) + kind + data
            + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF))
for i in range(30):
    raw = b"".join(b"\x00" + bytes([(i * 8) % 256, 200, (row * 4) % 256, 255] * w) for row in range(h))
    png = (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
           + chunk(b"IDAT", zlib.compress(raw, 6)) + chunk(b"IEND", b""))
    (root / f"b-{i:02}.png").write_bytes(png)
print("fixture B: 30 real pngs")
PYEOF
# S: セッション用 44件。mtimeを「img-00が最古」に設定 → date_descは
#    名前順の逆順になり、name順再採番との差が検出可能
python3 - "$WORK/home/S" <<'PYEOF'
import os, struct, sys, time, zlib
from pathlib import Path
root = Path(sys.argv[1]); root.mkdir(parents=True, exist_ok=True)
w = h = 48
def chunk(kind: bytes, data: bytes) -> bytes:
    return (struct.pack(">I", len(data)) + kind + data
            + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF))
base = time.time() - 44 * 3600
for i in range(44):
    raw = b"".join(b"\x00" + bytes([(row * 3) % 256, 120, (i * 40) % 256, 255] * w) for row in range(h))
    png = (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
           + chunk(b"IDAT", zlib.compress(raw, 6)) + chunk(b"IEND", b""))
    p = root / f"img-{i:02}.png"
    p.write_bytes(png)
    os.utime(p, (base + i * 3600, base + i * 3600))
print("fixture S: 44 pngs with staggered mtimes")
PYEOF

# ===== 1) 走査中のフォルダ切替 (Major-1) =====
echo "=== 1) 走査中に別フォルダへ切替 (/ 全FS走査中にBをクリック) ==="
# 注: xdotoolポインタ到達が環境により不安定なため、ツリー操作はdev bridgeの
# DOM click (本物のtree-labelボタン → selectFolder → open_folder生産経路)
# で駆動する (devtest.tsの設計意図と同じ)
launch "$WORK/home/B" "$WORK/home"
wait_log "scan metrics" 30 || fail "startup scan of B did not finish"
wait_log "devtest:" 30 || fail "dev bridge not flowing"
# ツリー2行目「ファイルシステム (/)」→ 全FS走査 (数秒以上かかる)
xdotool key ctrl+F7; sleep 1.5
grep -a "folder opened (adhoc scan started)" "$WORK/logs/app.log" | grep -aqF "folder=/ generation" \
  || fail "could not open / from tree (ctrl+F7)"
shot 01-scanning-root
pass "/ を走査開始 (全FS。深さ16ガードが効く本物の遅い走査)"
# 走査中にホーム直下のB (アルファベット最初の子) へ切替
xdotool key shift+F7
for _ in $(seq 1 20); do
  if grep -a "folder opened (adhoc scan started)" "$WORK/logs/app.log" | grep -aqF "$WORK/home/B"; then
    break
  fi
  sleep 0.5
done
grep -a "folder opened (adhoc scan started)" "$WORK/logs/app.log" | grep -aqF "$WORK/home/B" \
  || fail "tree click did not open folder B (shift+F7)"
grep -aq "cannot switch folder" "$WORK/logs/app.log" \
  && fail "old rejection error must not exist"
pass "走査中の切替がErrなく受理された (旧: cannot switch folder while scanning)"
wait_log "scan abandoned \(superseded by newer scan\)" 60 \
  || fail "/ scan was not superseded (did it finish too fast?)"
pass "旧走査は世代トークンで破棄 (scan abandoned)"
# Bの再走査完了 (起動時+切替後で2回) と最終一覧
wait_log "scan metrics root=.*home/B found=30" 60 || true
for _ in $(seq 1 30); do
  tot=$(devtest_value tot || true)
  [[ "$tot" == "30" ]] && break
  sleep 0.5
done
[[ "$tot" == "30" ]] || fail "expected tot=30 (B only), got ${tot:-none}"
b_metrics=$(count_log "scan metrics root=.*home/B")
[[ "$b_metrics" -ge 2 ]] || fail "B should be scanned twice (startup+switch), got $b_metrics"
root_metrics=$(count_log "scan metrics root=/ ")
[[ "$root_metrics" == "0" ]] || fail "abandoned / scan must not log completion metrics"
pass "切替後の一覧はBのみ (tot=30。/ の結果は混入しない・完了扱いでない)"
shot 02-switched-to-B
terminate_app

# ===== 1b) 走査キャンセル (Major-1) =====
echo "=== 1b) 走査中にキャンセル ==="
launch "$WORK/home/B" "$WORK/home"
wait_log "devtest:" 30 || fail "dev bridge not flowing"
xdotool key ctrl+F7; sleep 1.5
grep -a "folder opened (adhoc scan started)" "$WORK/logs/app.log" | grep -aqF "folder=/ generation" \
  || fail "could not open / from tree (ctrl+F7)"
sleep 1.5   # 走査が進んでいる状態で
xdotool key shift+F6
wait_log "adhoc scan cancelled" 30 || fail "cancel did not reach backend"
pass "走査キャンセルがbackendへ到達 (adhoc scan cancelled)"
sleep 3
root_metrics=$(count_log "scan metrics root=/ ")
[[ "$root_metrics" == "0" ]] || fail "cancelled / scan must not log completion metrics"
pass "キャンセルで完了扱いにならない (scan metrics なし・部分結果で停止)"
shot 03-cancelled
terminate_app

# ===== 2) adhocセッション再開 (Major-2) =====
echo "=== 2) mtimeソート→セッション→再起動→再開 ==="
launch "$WORK/home/S" "$WORK/home"
wait_log "scan metrics" 60 || fail "scan of S did not complete"
wait_log "devtest:" 30 || fail "dev bridge not flowing"
xdotool key ctrl+F2; sleep 0.3   # グリッドフォーカス
xdotool key shift+F5; sleep 1.0  # ソート: name → date_desc
for _ in $(seq 1 30); do
  srt=$(devtest_value srt || true)
  [[ "$srt" == "date_desc" ]] && break
  sleep 0.5
done
[[ "$srt" == "date_desc" ]] || fail "sort did not switch to date_desc (srt=${srt:-none})"
pass "ソートを更新日時 (date_desc) へ切替"
xdotool key shift+F2; sleep 1.0  # この一覧でセッション開始
act=$(devtest_value act || true)
[[ "$act" == "1" ]] || fail "session did not start (act=${act:-none})"
xdotool key shift+F4; sleep 0.5  # 次へ →
xdotool key shift+F4; sleep 1.0
pos=$(devtest_value pos || true); cur=$(devtest_value cur || true); fn=$(devtest_value fn || true)
[[ "$pos" == "2" ]] || fail "expected session pos=2, got ${pos:-none}"
[[ "$fn" == "img-41.png" ]] || fail "date_desc pos2 should be img-41.png, got ${fn:-none}"
pass "セッション位置2 = $fn (cur=$cur)"
shot 04-session-date-desc
terminate_app

echo "--- 再起動 (既定はname順。セッションはlocalStorageに残る) ---"
launch "$WORK/home/S" "$WORK/home"
wait_log "scan metrics" 60 || fail "rescan did not complete"
wait_log "devtest:" 30 || fail "dev bridge not flowing"
for _ in $(seq 1 30); do
  srt=$(devtest_value srt || true)
  [[ "$srt" == "name" ]] && break
  sleep 0.5
done
[[ "$srt" == "name" ]] || fail "fresh boot should be name sort (srt=${srt:-none})"
pass "再起動直後はname順 (問題の前提を確認)"
shot 05-reboot-name-sort
xdotool key shift+F3; sleep 1.0  # 再開
# 復元 (open_folder + date_desc) → 走査完了待ち → 検証 → フォーカス
for _ in $(seq 1 120); do
  act=$(devtest_value act || true); srt=$(devtest_value srt || true)
  fn2=$(devtest_value fn || true)
  if [[ "$act" == "1" && "$srt" == "date_desc" && "$fn2" == "$fn" ]]; then break; fi
  sleep 0.5
done
act=$(devtest_value act || true); pos2=$(devtest_value pos || true)
cur2=$(devtest_value cur || true); fn2=$(devtest_value fn || true)
[[ "$act" == "1" ]] || fail "session did not resume (act=${act:-none})"
[[ "$pos2" == "2" && "$cur2" == "$cur" && "$fn2" == "$fn" ]] \
  || fail "resume landed elsewhere: pos=$pos2 cur=$cur2 fn=$fn2 (want pos=2 cur=$cur fn=$fn)"
pass "再開が正しい画像を指す (pos=$pos2 cur=$cur2 → $fn2)"
shot 06-resumed-correct-image
terminate_app

# ===== 2b) 件数不一致警告 (Major-2) =====
echo "=== 2b) ファイル削除 (>5%ずれ) 後の再開は警告で中止 ==="
rm "$WORK/home/S"/img-0*.png "$WORK/home/S"/img-1*.png   # 20件削除 (44→24)
launch "$WORK/home/S" "$WORK/home"
wait_log "scan metrics" 60 || fail "scan did not complete"
wait_log "devtest:" 30 || fail "dev bridge not flowing"
xdotool key shift+F3; sleep 8   # 再開 (復元→走査完了→検証)
act=$(devtest_value act || true)
[[ "$act" == "0" ]] || fail "resume should be aborted on count drift (act=${act:-none})"
# 警告文言はS行noteから直接確認 (日本語はdevtest_valueの抽出対象外)
if ! grep -a "devtest:" "$WORK/logs/app.log" | tail -1 | grep -aq "再開を中止"; then
  fail "abort warning missing in devtest S line: $(grep -a 'devtest:' "$WORK/logs/app.log" | tail -1 | grep -oE 'S act=[^|]*' || true)"
fi
pass "件数ずれで再開中止 + 警告表示 (act=0)"
shot 07-count-drift-aborted

echo
echo "QA-WP4 E2E RESULT: PASS ($pass_count項目)"
echo "artifacts: $WORK (logs + shots)"
