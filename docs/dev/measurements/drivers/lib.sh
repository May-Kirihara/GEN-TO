#!/usr/bin/env bash
# WP-7 計測ドライバ共通ライブラリ (source して使う)
# 環境: DISPLAY=:1 / X11。リリースバイナリ + GENTO_DEV_BRIDGE=1 で起動する。

BIN=/mnt/holyland/Project/GEN-TO/target/release/gento-tauri
DATASET=/mnt/holyland/Project/GEN-TO/sample/dataset
WP7=/tmp/opencode/wp7
export DISPLAY=:1

commit_id=$(git -C /mnt/holyland/Project/GEN-TO rev-parse --short HEAD)

# launch_app <cache_dir> <log_file> [extra env...]
launch_app() {
  local cache_dir="$1" log_file="$2"
  shift 2
  env GENTO_LOG=info GENTO_DEV_BRIDGE=1 NO_COLOR=1 "$@" "$BIN" \
    --dataset "$DATASET" --cache-dir "$cache_dir" >>"$log_file" 2>&1 &
  APP_PID=$!
  # ウィンドウが現れるのを待つ
  for _ in $(seq 1 100); do
    if xdotool search --onlyvisible --pid "$APP_PID" getwindowname %@ >/dev/null 2>&1; then
      break
    fi
    sleep 0.2
  done
  sleep 1.5
  WID=$(xdotool search --onlyvisible --pid "$APP_PID" 2>/dev/null | head -1 || true)
  if [ -n "$WID" ]; then
    xdotool windowactivate --sync "$WID" 2>/dev/null || true
  fi
  echo "launched pid=$APP_PID wid=$WID log=$log_file"
}

# アプリが終了するまで待つ (graceful path)
wait_app_exit() {
  local timeout="${1:-30}"
  for _ in $(seq 1 $((timeout * 10))); do
    kill -0 "$APP_PID" 2>/dev/null || return 0
    sleep 0.1
  done
  return 1
}

kill_app_hard() {
  kill -9 "$APP_PID" 2>/dev/null || true
  pkill -9 -P "$APP_PID" 2>/dev/null || true
}

count_tmp_files() {
  find "$1" -name '.tmp-*' 2>/dev/null | wc -l
}

# devtest行をログから待つ: wait_devtest <log> <grep-pattern> [timeout_s]
wait_devtest() {
  local log="$1" pattern="$2" timeout="${3:-60}"
  local deadline=$((SECONDS + timeout))
  while [ $SECONDS -lt $deadline ]; do
    if sed 's/\x1b\[[0-9;]*m//g' "$log" 2>/dev/null | grep -a "devtest:" | tail -1 | grep -q -- "$pattern"; then
      return 0
    fi
    sleep 0.3
  done
  return 1
}

# 直近のdevtest行からキーを取り出す: devtest_value <log> <key>
devtest_value() {
  local log="$1" key="$2"
  # ANSI残照応に頑健に: 色コードを剥いてから抽出
  sed 's/\x1b\[[0-9;]*m//g' "$log" 2>/dev/null | grep -a "devtest:" | tail -1 \
    | grep -o -- "${key}=[-0-9.?]*" | tail -1 | cut -d= -f2
}

screenshot() {
  import -window root "$WP7/shots/$1.png" 2>/dev/null || import "$WP7/shots/$1.png"
}
