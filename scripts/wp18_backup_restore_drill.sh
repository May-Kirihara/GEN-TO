#!/usr/bin/env bash
# WP-18-1 D3: バックアップ復元演習 (実アプリE2E — uxlib1流儀)。
#
# 実行: bash scripts/wp18_backup_restore_drill.sh
#  (事前に pnpm --dir apps/desktop build および
#   cargo build --release -p gento-tauri --features custom-protocol,devtools)
#
# 演習の流れ (計画書§26「バックアップからの復元演習」):
#   P1) 登録 (実キー入力でルート追加) → ハッシュ → ★5評価 + タグ付与
#   P2) ポータブルバックアップ作成 (dev bridge 6 = 生産command経由)
#   P3) アンインストール相当: DB (+WAL/SHM) とサムネイルキャッシュを削除
#       — .gentoバックアップと元画像は残す (OS慣行: データは残る設計)
#   P4) 再起動 (=新規DBをmigrationで作成) → 同ルート再登録 → 再ハッシュ
#       — 評価/タグが失われた状態 (データ消失の実証) を確認
#   P5) 復元 (dev bridge 7 = plan→確認トークン→execute. RestoreDialogと
#       同一command列)
#   P6) 再起動して復元結果が永続化されていることをUI実測 (rat=5 tags=1)
#
# この演習で機械的にカバーできない部分 (debアンインストーラ実行・
# デスクトップエントリ削除等) は docs/handoff/2026-09-04-mvp-wp18-1.md
# の手順書を参照 (DB削除はアプリデータ消失の本体であり、ここで等価に
# 再現している)。
#
# 運用メモ: 専用Xディスプレイ (既定 :6 = Xephyr。無ければ起動)。
# dev bridgeの機械可読行 (app.logの devtest: 行) で検証する。
set -uo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO=$(cd "$SCRIPT_DIR/.." && pwd)
BIN="$REPO/target/release/gento-tauri"
WORK=/tmp/opencode/wp18/drill
DISPLAY="${GENTO_E2E_DISPLAY:-:6}"
export NO_COLOR=1 GENTO_LOG=info GENTO_DEV_BRIDGE=1
TAG_NAME="restore-drill-tag"

# Xephyr起動時のホスト画面は環境のDISPLAY (例: :1)。Xephyr自身が開く
# 画面が $DISPLAY (既定 :6) — 両者を混同しない (DISPLAYはexport済み
# 変数のため、ここでの代入は子プロセスへ伝播する点に注意)。
if ! xdpyinfo -display "$DISPLAY" >/dev/null 2>&1; then
  setsid nohup env DISPLAY="${HOST_DISPLAY:-:0}" Xephyr "$DISPLAY" -screen 1600x1000 \
    >/tmp/opencode/wp18-xephyr.log 2>&1 </dev/null &
  for _ in $(seq 1 50); do
    xdpyinfo -display "$DISPLAY" >/dev/null 2>&1 && break
    sleep 0.2
  done
fi
xdpyinfo -display "$DISPLAY" >/dev/null 2>&1 || { echo "no X display on $DISPLAY" >&2; exit 1; }

rm -rf "$WORK"; mkdir -p "$WORK/home" "$WORK/env/thumbs" "$WORK/env/db" "$WORK/dataset" \
  "$WORK/shots" "$WORK/logs"
DB="$WORK/env/db/library.sqlite3"
BKDIR="$WORK/env/db/db-backups"
SHOT="$WORK/shots"
NOTE="$WORK/drill-notes.txt"; : > "$NOTE"
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
wait_log() { # <pattern> <timeout_s>
  local deadline=$((SECONDS + ${2:-30}))
  while (( SECONDS < deadline )); do
    grep -aqE -- "$1" "$WORK/logs/app.log" && return 0
    sleep 0.2
  done
  return 1
}

# fixture: 実PNG 4枚 (uxlib1と同じグラデーション生成)
python3 - "$WORK/dataset" <<'PYEOF'
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
dataset = Path(sys.argv[1])
for i in range(4):
    png(dataset / f"wp18-{i:02}.png", i)
print("fixture: dataset 4 pngs")
PYEOF

APP_PID=""
launch() {
  : > "$WORK/logs/app.log"
  HOME="$WORK/home" XDG_DATA_HOME="$WORK/home/.local/share" \
    XDG_CACHE_HOME="$WORK/home/.cache" \
    "$BIN" --dataset "$WORK/dataset" \
      --cache-dir "$WORK/env/thumbs" --db "$DB" \
      >> "$WORK/logs/app.log" 2>&1 &
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
graceful_stop() {
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

# ========== P1) 登録 → 評価★5 → タグ付与 ==========
launch
wait_log "devtest:" 30 || fail "P1: dev bridge not flowing"
xdotool key ctrl+shift+F2; sleep 0.4          # 登録ルートパス入力へフォーカス
xdotool type --delay 25 "$WORK/dataset"; sleep 0.3
xdotool key Return; sleep 1.5
wait_value mode library 20 && pass "P1: ルート追加 → library表示" \
  || fail "P1: library切替なし (mode=$(devtest_value mode || echo none))"
wait_value tot 4 20 || fail "P1: 4件 (tot=$(devtest_value tot || echo none))"
xdotool key ctrl+shift+F4; sleep 1.0          # focusCell0
wait_value fdb asset 60 || fail "P1: ハッシュ完了待ち (fdb=$(devtest_value fdb || echo -))"
wait_value edit 1 10 && pass "P1: ハッシュ後の編集可" \
  || fail "P1: 編集不可 (edit=$(devtest_value edit || echo -))"
xdotool key ctrl+F4; sleep 2.0                # metaRate5
wait_value rat 5 20 && pass "P1: ★5付与" \
  || fail "P1: ★5失敗 (rat=$(devtest_value rat || echo -))"
xdotool key ctrl+shift+F3; sleep 0.4          # focusTagInput
xdotool type --delay 25 "$TAG_NAME"; sleep 0.3
xdotool key Return; sleep 1.5                 # EnterでsubmitTags
wait_value tags 1 20 && pass "P1: タグ付与 (tags=1)" \
  || fail "P1: タグ失敗 (tags=$(devtest_value tags || echo -))"
shot 01-registered-rated-tagged

# ========== P2) ポータブルバックアップ作成 ==========
xdotool key ctrl+shift+F12; sleep 0.4         # foCmdMode
xdotool key 6; sleep 1.0                      # bkPortableCreateFirstRoot
bk_ok=0
for _ in $(seq 1 100); do
  [[ "$(devtest_value bok || true)" == "1" && "$(devtest_value bop || true)" == "create" ]] && { bk_ok=1; break; }
  sleep 0.3
done
if (( bk_ok )) && [[ "$(devtest_value ba || echo 0)" -ge 4 ]]; then
  pass "P2: .gento作成 (ba=$(devtest_value ba) bf=$(devtest_value bf) bc=$(devtest_value bc))"
else
  fail "P2: 作成失敗 (bop=$(devtest_value bop || echo -) bok=$(devtest_value bok || echo -) berr=$(devtest_value berr || echo -))"
fi
GENTO_DIR=$(find "$BKDIR" -maxdepth 1 -name "*.gento" -type d | head -1)
if [[ -n "$GENTO_DIR" && -f "$GENTO_DIR/backup-info.json" ]] \
  && grep -q '"complete": *true' "$GENTO_DIR/backup-info.json"; then
  pass "P2: バックアップ検証 (backup-info.json complete=true @ ${GENTO_DIR#$WORK/})"
else
  fail "P2: backup-info.json 不全 (dir=$GENTO_DIR)"
fi
shot 02-backup-created

# ========== P3) アンインストール相当: DB+キャッシュ削除 (.gentoは残す) ==========
graceful_stop && pass "P3: SIGTERM graceful終了" || fail "P3: graceful終了せず"
rm -f "$DB" "$DB-wal" "$DB-shm"
rm -rf "$WORK/env/thumbs"; mkdir -p "$WORK/env/thumbs"
[[ ! -e "$DB" && -d "$GENTO_DIR" ]] && pass "P3: DB/キャッシュ削除・バックアップは残存" \
  || fail "P3: 削除/残存確認失敗"

# ========== P4) 再起動 (新規DB) → 再登録 → 消失実証 → 復元 ==========
launch
wait_log "devtest:" 30 || fail "P4: dev bridge not flowing (再起動)"
xdotool key ctrl+shift+F2; sleep 0.4
xdotool type --delay 25 "$WORK/dataset"; sleep 0.3
xdotool key Return; sleep 1.5
wait_value mode library 20 || fail "P4: library表示にならない"
wait_value tot 4 40 || fail "P4: 4件 (tot=$(devtest_value tot || echo none))"
xdotool key ctrl+shift+F4; sleep 1.0
wait_value fdb asset 60 || fail "P4: 再ハッシュ完了待ち (fdb=$(devtest_value fdb || echo -))"
if wait_value tags 0 8 && [[ "$(devtest_value rat || echo -)" == "0" ]]; then
  pass "P4: 復元前は評価/タグ無し (rat=0 tags=0 — 消失の実証)"
else
  fail "P4: 復元前状態が期待と違う (rat=$(devtest_value rat || echo -) tags=$(devtest_value tags || echo -))"
fi
shot 03-fresh-db-after-uninstall-equivalent

xdotool key ctrl+shift+F12; sleep 0.4         # foCmdMode
xdotool key 7; sleep 1.0                      # bkRestoreLatest
restored=0
for _ in $(seq 1 100); do
  [[ "$(devtest_value bok || true)" == "1" && "$(devtest_value bop || true)" == "restore" ]] && { restored=1; break; }
  [[ "$(devtest_value bop || true)" == "restore" && "$(devtest_value bok || true)" == "0" ]] && break
  sleep 0.3
done
if (( restored )) && [[ "$(devtest_value bap || echo 0)" -ge 4 && "$(devtest_value bsk || echo 1)" == "0" ]]; then
  pass "P5: 復元実行 (bap=$(devtest_value bap) bsk=$(devtest_value bsk))"
else
  fail "P5: 復元失敗 (bop=$(devtest_value bop || echo -) bok=$(devtest_value bok || echo -) bap=$(devtest_value bap || echo -) bsk=$(devtest_value bsk || echo -) berr=$(devtest_value berr || echo -))"
fi

# ========== P6) 再起動 → 復元の永続化をUI実測 ==========
graceful_stop || fail "P6: graceful終了せず (復元後)"
launch
wait_log "devtest:" 30 || fail "P6: dev bridge not flowing"
wait_value mode library 30 || fail "P6: library起動でない"
wait_value tot 4 40 || fail "P6: 4件"
xdotool key ctrl+shift+F4; sleep 1.0
wait_value fdb asset 60 || fail "P6: ハッシュ待ち"
wait_value rat 5 20 && pass "P6: ★5復元 (rat=5)" \
  || fail "P6: ★5未復元 (rat=$(devtest_value rat || echo -))"
wait_value tags 1 20 && pass "P6: タグ復元 (tags=1)" \
  || fail "P6: タグ未復元 (tags=$(devtest_value tags || echo -))"
shot 04-restored-rated-tagged

graceful_stop && pass "P6: SIGTERM graceful終了 (演習完了)" || fail "P6: 終了せず"

echo
echo "=== WP-18-1 D3 復元演習: FAILURES=$FAILURES ==="
cat "$NOTE"
[[ $FAILURES -eq 0 ]] || exit 1
