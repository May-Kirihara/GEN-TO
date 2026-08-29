#!/usr/bin/env bash
# WP-7 計測補助: gento-tauri本体 + WebKit子プロセスのRSS合計を /proc から測る。
# 使い方: rss-sample.sh <pid> [label]
# 出力: "<iso8601> <label> pid=<pid> rss_total_kb=<n> procs=<n> rss_main_kb=<n> rss_children_kb=<n>"
set -u
pid="${1:?pid required}"
label="${2:-sample}"

total_kb=0
main_kb=0
child_kb=0
count=0

# 子プロセス (WebKitWebProcess等) を再帰的に集める
collect() {
  local p="$1" kb
  kb=$(awk '/VmRSS/{print $2}' "/proc/$p/status" 2>/dev/null || echo 0)
  if [ "$kb" -gt 0 ]; then
    total_kb=$((total_kb + kb))
    count=$((count + 1))
    if [ "$p" = "$pid" ]; then main_kb=$kb; else child_kb=$((child_kb + kb)); fi
  fi
  for c in $(pgrep -P "$p" 2>/dev/null); do
    collect "$c"
  done
}

collect "$pid"
echo "$(date -Is) ${label} pid=${pid} rss_total_kb=${total_kb} procs=${count} rss_main_kb=${main_kb} rss_children_kb=${child_kb}"
