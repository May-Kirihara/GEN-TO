#!/usr/bin/env bash
# WP-0 releaseスモーク: devtools feature隔離のartifact-level証拠。
#
# 使い方:
#   scripts/release-smoke.sh            # フル実行 (build + バイナリ検査 + 実行時検証)
#
# 検証内容 (受け入れ条件3/4/5/6の証跡):
#   (a) featureなしrelease buildのバイナリにdev系command識別子が存在しない
#       (devtest_report / set_failure_injection / dev_bridge_enabled /
#        GENTO_DEV_BRIDGE / 仮想カタログ識別文字列)
#   (b) --features devtools付きrelease buildで計測シナリオが動作する
#       (起動 → devtestログ出力 → スキャン → サムネイル生成数件 → graceful終了)
#   (c) featureなしrelease buildがデータセットなしで起動し、空のUIを表示、
#       SIGTERMでgraceful終了する。GENTO_DEV_BRIDGE=1を与えても
#       devtestログが出ない (=ランタイムでもdev機能が不在) ことを確認
#
# 要件: cargo / pnpm / jq不要 / xdotool / Xディスプレイ (既定 :1)
set -euo pipefail

REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
BIN="$REPO_ROOT/target/release/gento-tauri"
DESKTOP_DIR="$REPO_ROOT/apps/desktop"
DISPLAY="${DISPLAY:-:1}"
export DISPLAY NO_COLOR=1 GENTO_LOG=info

WORK="${SMOKE_WORK:-$(mktemp -d /tmp/gento-wp0-smoke.XXXXXX)}"
mkdir -p "$WORK"
STEP=0

cleanup() {
  if [[ -n "${APP_PID:-}" ]] && kill -0 "$APP_PID" 2>/dev/null; then
    kill -9 "$APP_PID" 2>/dev/null || true
  fi
  if [[ "${SMOKE_KEEP:-0}" != "1" && "$WORK" == /tmp/gento-wp0-smoke.* ]]; then
    rm -rf "$WORK"
  fi
}
trap cleanup EXIT

pass() { echo "[PASS] $*"; }
fail() { echo "[FAIL] $*" >&2; exit 1; }
step() { STEP=$((STEP + 1)); echo; echo "=== Step $STEP: $* ==="; }

# バイナリ内に文字列があるか (grep -a: バイナリをテキスト扱い)
has_string() { grep -aqF -- "$2" "$1"; }

wait_log() { # wait_log <log> <pattern> <timeout_s> [説明]
  local log=$1 pattern=$2 timeout=${3:-30} desc=${4:-"$2"}
  local deadline=$((SECONDS + timeout))
  while (( SECONDS < deadline )); do
    if grep -aqE -- "$pattern" "$log"; then return 0; fi
    sleep 0.2
  done
  fail "timeout waiting for '$desc' in $log"
}

# devtest行から最新スナップショットの数値を取り出す: devtest_value <log> <key>
# (\b で "pgen=" 内の "gen=" 誤一致を防ぐ)
devtest_value() { grep -a "devtest:" "$1" | tail -1 | grep -oE "\b$2=[-0-9.]+" | head -1 | cut -d= -f2; }

launch_app() { # launch_app <binary> <dataset> <cache> <log> [GENTO_DEV_BRIDGE] [screenshot名]
  local binary=$1 dataset=$2 cache=$3 log=$4 bridge=${5:-0} shot=${6:-}
  : > "$log"
  GENTO_DEV_BRIDGE="$bridge" "$binary" --dataset "$dataset" --cache-dir "$cache" \
    >> "$log" 2>&1 &
  APP_PID=$!
  # ウィンドウ出現 + フォーカス (xdotoolキー注入に必要)
  local wid=""
  local deadline=$((SECONDS + 30))
  while (( SECONDS < deadline )); do
    wid=$(xdotool search --onlyvisible --pid "$APP_PID" 2>/dev/null | head -1 || true)
    [[ -n "$wid" ]] && break
    kill -0 "$APP_PID" 2>/dev/null || fail "app exited before window creation (see $log)"
    sleep 0.2
  done
  [[ -n "$wid" ]] || fail "app window not found (see $log)"
  APP_WID=$wid
  xdotool windowactivate --sync "$wid" >/dev/null 2>&1 || true
  # 人的確認用スクリーンショット (任意。失敗してもスキップ)。
  # webviewの初回描画まで時間がかかるため十分待つ。
  if [[ -n "$shot" ]] && command -v import >/dev/null; then
    sleep 5
    import -window "$wid" "$WORK/$shot.png" 2>/dev/null || true
  fi
}

terminate_app() { # terminate_app <log> -> exit code 0 の確認
  kill -TERM "$APP_PID"
  local deadline=$((SECONDS + 20))
  while (( SECONDS < deadline )); do
    if ! kill -0 "$APP_PID" 2>/dev/null; then
      wait "$APP_PID" || fail "app did not exit with status 0"
      pass "graceful exit (SIGTERM, status 0)"
      APP_PID=""
      return 0
    fi
    sleep 0.2
  done
  fail "app did not exit within 20s after SIGTERM"
}

# テスト用PNGデータセット (stdlibのみ。sample/は読み取らない規約のため)
make_fixture_dataset() { # make_fixture_dataset <dir> <count>
  python3 - "$1" "$2" <<'PYEOF'
import struct, sys, zlib
from pathlib import Path

root = Path(sys.argv[1]); count = int(sys.argv[2])
root.mkdir(parents=True, exist_ok=True)
w = h = 96

def chunk(kind: bytes, data: bytes) -> bytes:
    return (struct.pack(">I", len(data)) + kind + data
            + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF))

for i in range(count):
    raw = bytearray()
    for y in range(h):
        raw.append(0)  # PNGフィルタ: None
        for x in range(w):
            raw += bytes([(x * 2 + i * 40) % 256, (y * 3) % 256, (i * 80) % 256, 255])
    png = (b"\x89PNG\r\n\x1a\n"
           + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
           + chunk(b"IDAT", zlib.compress(bytes(raw), 6))
           + chunk(b"IEND", b""))
    (root / f"img-{i:03}.png").write_bytes(png)
print(f"fixture: {count} pngs in {root}")
PYEOF
}

command -v xdotool >/dev/null || fail "xdotool is required"
command -v python3 >/dev/null || fail "python3 is required"
xdotool getdisplaygeometry >/dev/null 2>&1 || fail "X display '$DISPLAY' is not reachable"
# スモークはfrontendDistを埋め込んだproductionビルドで検証する。
# vite devサーバー (localhost:5173) が立ち上がっていると結果が汚染されるため排除。
if curl -sf -o /dev/null http://localhost:5173/ 2>/dev/null || ss -ltn 2>/dev/null | grep -q :5173; then
  fail "port 5173 (vite dev server) is listening; stop 'pnpm tauri dev' before running this smoke"
fi
echo "smoke work dir: $WORK"

# ============================================================

step "frontend build (apps/desktop)"
(cd "$DESKTOP_DIR" && pnpm build >"$WORK/build-frontend.txt" 2>&1)
pass "pnpm build"

step "release build WITHOUT devtools (cargo build --release + custom-protocol)"
# custom-protocol: tauri CLI (pnpm tauri build) が自動で有効化するfeature。
# cargo直接ビルドでは明示が必要 (無いとfrontendDistでなくdevUrl=localhost:5173
# を見に行き、vite無しではUIが起動しない)。
cargo build --release -p gento-tauri --features custom-protocol \
  --manifest-path "$REPO_ROOT/Cargo.toml" >"$WORK/build-prod.txt" 2>&1
if grep -qE "^warning" "$WORK/build-prod.txt"; then
  grep -E "^warning" -A4 "$WORK/build-prod.txt" >&2
  fail "featureless release build emitted warnings"
fi
[[ -x "$BIN" ]] || fail "binary not found: $BIN"
pass "clean build: $BIN"

step "(a) artifact check: dev識別子がfeatureなしバイナリに存在しないこと"
# 必在 (検出器の健全性確認用コントロール)
for present in "get_catalog_summary" "request_thumbnails" "gento-thumb" "clear_thumbnail_cache"; do
  has_string "$BIN" "$present" || fail "control string missing (検出器不良?): $present"
done
pass "control strings present (production commands)"
# 不在 (devtools機能)
for absent in "devtest_report" "set_failure_injection" "dev_bridge_enabled" \
              "GENTO_DEV_BRIDGE" "set_catalog_mode" \
              "仮想カタログへ切替する前にスキャンを実行してください" "virtual-" "injected failure"; do
  if has_string "$BIN" "$absent"; then
    fail "dev identifier leaked into featureless release binary: $absent"
  fi
done
pass "dev identifiers absent: devtest_report / set_failure_injection / dev_bridge_enabled / GENTO_DEV_BRIDGE / set_catalog_mode / virtual- / injected failure"

step "(c) runtime: featureなしバイナリがデータセットなしで起動する (GENTO_DEV_BRIDGE=1を与えても)"
launch_app "$BIN" "$WORK/no-such-dataset" "$WORK/cache-prod" "$WORK/prod-empty.log" 1 "prod-empty-ui"
wait_log "$WORK/prod-empty.log" "main window created" 30 "main window"
wait_log "$WORK/prod-empty.log" "dataset resolved" 10 "dataset resolved"
grep -aq "exists=false" "$WORK/prod-empty.log" \
  || fail "dataset resolved line must show exists=false"
pass "dataset resolved with exists=false"
sleep 4
kill -0 "$APP_PID" 2>/dev/null || fail "app crashed on empty dataset (see $WORK/prod-empty.log)"
pass "process alive with empty dataset (no crash)"
if grep -aq "devtest:" "$WORK/prod-empty.log"; then
  fail "devtest lines must not appear in featureless build even with GENTO_DEV_BRIDGE=1"
fi
pass "no devtest: lines even with GENTO_DEV_BRIDGE=1 (compile-time absence)"
terminate_app "$WORK/prod-empty.log"

step "release build WITH devtools (cargo build --release --features custom-protocol,devtools)"
cargo build --release -p gento-tauri --features custom-protocol,devtools \
  --manifest-path "$REPO_ROOT/Cargo.toml" >"$WORK/build-devtools.txt" 2>&1
if grep -qE "^warning" "$WORK/build-devtools.txt"; then
  grep -E "^warning" -A4 "$WORK/build-devtools.txt" >&2
  fail "devtools release build emitted warnings"
fi
pass "clean devtools build"

step "(b-pre) artifact check: dev識別子がdevtoolsバイナリに存在すること (対照)"
for present in "devtest_report" "set_failure_injection" "dev_bridge_enabled" "set_catalog_mode"; do
  has_string "$BIN" "$present" || fail "dev identifier missing in devtools build: $present"
done
pass "dev identifiers present in devtools build"

step "(b) runtime: devtoolsビルドで計測シナリオ (起動→スキャン→サムネイル生成→終了)"
make_fixture_dataset "$WORK/dataset" 6
launch_app "$BIN" "$WORK/dataset" "$WORK/cache-devtools" "$WORK/devtools-run.log" 1 "devtools-ui"
wait_log "$WORK/devtools-run.log" "devtest:" 60 "devtest report lines"
pass "dev bridge active (devtest: lines flowing)"
xdotool key F8   # スキャン開始 (devtest action)
wait_log "$WORK/devtools-run.log" "scan metrics" 60 "scan metrics log"
pass "scan completed (scan metrics logged)"
{
  # tot=6 (走査結果) と gen>=1 (サムネイル生成数件) を待つ。
  # グリッドの需要駆動で生成されるためキー押下は不要なのが本来だが、
  # 環境依存の揺れに備えてctrl+F8 (全件生成) のフォールバックを置く。
  ok_tot=0; ok_gen=0; gen=""
  deadline=$((SECONDS + 60)); sent_fallback=0
  while (( SECONDS < deadline )); do
    tot=$(devtest_value "$WORK/devtools-run.log" tot)
    if [[ "$tot" =~ ^[0-9]+$ ]] && (( tot >= 6 )); then ok_tot=1; fi
    gen=$(devtest_value "$WORK/devtools-run.log" gen)
    if [[ "$gen" =~ ^[0-9]+$ ]] && (( gen >= 1 )); then ok_gen=1; fi
    if (( ok_tot && ok_gen )); then break; fi
    if (( ok_tot && !ok_gen )) && (( SECONDS > deadline - 45 )) && (( !sent_fallback )); then
      xdotool key ctrl+F8; sent_fallback=1
    fi
    sleep 0.5
  done
  if (( ! ok_tot )); then fail "catalog total did not reach 6 (devtest tot)"; fi
  if (( ! ok_gen )); then fail "no thumbnail generated (devtest gen)"; fi
  echo "generated thumbnails: gen=$gen"
}
pass "thumbnails generated (scan + demand-driven generation)"
terminate_app "$WORK/devtools-run.log"

echo
echo "SMOKE RESULT: PASS (all steps)"
echo "artifacts: $WORK (SMOKE_KEEP=1で保全)"
