#!/usr/bin/env bash
# A4 終了安全の実証: 生成中にSIGTERM → 終了後 .tmp-* が残らないことを3回確認。
set -u
. /tmp/opencode/wp7/lib.sh

pass=0
fail=0
for i in 1 2 3; do
  cache="$WP7/cache-sigterm-$i"
  rm -rf "$cache"
  log="$WP7/logs/sigterm-$i.log"
  : >"$log"
  launch_app "$cache" "$log"
  xdotool key F8            # スキャン開始
  sleep 3                   # 走査完了待ち (1615件)
  xdotool key ctrl+F8       # 全件生成
  # 生成が始まり、かつキューに滞留している状態 (background 1500件超) を待つ
  ok=0
  for _ in $(seq 1 150); do
    gen=$(devtest_value "$log" "T gen")
    qb=$(devtest_value "$log" "qb")
    if [ "${gen:-0}" -gt 100 ] 2>/dev/null && [ "${qb:-0}" -gt 300 ] 2>/dev/null; then
      ok=1
      break
    fi
    sleep 0.2
  done
  if [ "$ok" != "1" ]; then
    echo "run$i: FAIL (generation never started)"
    fail=$((fail + 1))
    kill_app_hard
    continue
  fi
  kill -TERM "$APP_PID"     # 生成中にSIGTERM
  if wait_app_exit 30; then
    exited=graceful
  else
    exited=timeout
    kill_app_hard
  fi
  sleep 0.5
  tmp_count=$(count_tmp_files "$cache")
  graceful_log=$(grep -c "termination signal received" "$log" || true)
  shutdown_log=$(grep -c "shutting down thumbnail service" "$log" || true)
  last_gen=$(grep -a "devtest:" "$log" | tail -1 | grep -o 'T gen=[0-9]*' || true)
  echo "run$i: exit=$exited tmp_files=$tmp_count gen_at_kill~$last_gen sig_log=$graceful_log shutdown_log=$shutdown_log"
  if [ "$tmp_count" -eq 0 ] && [ "$exited" = "graceful" ]; then
    pass=$((pass + 1))
  else
    fail=$((fail + 1))
  fi
done
echo "RESULT: sigterm-safety pass=$pass fail=$fail (of 3)"
