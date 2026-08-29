#!/usr/bin/env bash
# A3 エラー注入の実機実証: decode/cache_write 30% 注入で
# (1) 失敗率が閾値どおり (2) キュー全体停止なし (3) .tmp残存なし
# (4) rate=0で復帰 — を1回のセッションで確認する。
set -u
. /tmp/opencode/wp7/lib.sh

cache="$WP7/cache-inject"
rm -rf "$cache"
log="$WP7/logs/a3-inject.log"
: >"$log"
launch_app "$cache" "$log"
xdotool key F8
wait_devtest "$log" "tot=1615" 30 || { echo "FAIL: scan"; kill_app_hard; exit 1; }

echo "--- decode 30% 注入"
xdotool key F11   # none -> decode 0.30
sleep 1
xdotool key ctrl+F8
# 全件処理完了 (キューが空になり、生成+失敗が1615に到達) を待つ
for _ in $(seq 1 300); do
  gen=$(devtest_value "$log" "T gen"); q=$(devtest_value "$log" "q=")
  fail=$(devtest_value "$log" "fail")
  if [ "${q:-1}" = "0" ] 2>/dev/null && [ "$(( ${gen:-0} + ${fail:-0} ))" -ge 1615 ] 2>/dev/null; then
    break
  fi
  sleep 0.3
done
decode_gen=$(devtest_value "$log" "T gen"); decode_fail=$(devtest_value "$log" "fail")
decode_inj=$(devtest_value "$log" "T inj")
alive=$(kill -0 $APP_PID 2>/dev/null && echo yes || echo no)
tmp1=$(count_tmp_files "$cache")
echo "decode30: inj=$decode_inj gen=$decode_gen fail=$decode_fail alive=$alive tmp=$tmp1"

echo "--- cache_write 30% 注入 (キャッシュ全削除 → 再生成)"
xdotool key F11   # decode -> cache_write 0.30
sleep 1
xdotool key ctrl+F9   # キャッシュ全削除
sleep 2
xdotool key ctrl+F8
for _ in $(seq 1 300); do
  gen=$(devtest_value "$log" "T gen"); q=$(devtest_value "$log" "q=")
  fail=$(devtest_value "$log" "fail")
  if [ "${q:-1}" = "0" ] 2>/dev/null && [ "$(( ${gen:-0} + ${fail:-0} ))" -ge 1615 ] 2>/dev/null; then
    break
  fi
  sleep 0.3
done
cw_gen=$(devtest_value "$log" "T gen"); cw_fail=$(devtest_value "$log" "fail")
cw_inj=$(devtest_value "$log" "T inj")
alive=$(kill -0 $APP_PID 2>/dev/null && echo yes || echo no)
tmp2=$(count_tmp_files "$cache")
echo "cacheWrite30: inj=$cw_inj gen=$cw_gen fail=$cw_fail alive=$alive tmp=$tmp2"

echo "--- 復帰 (注入なし + 再生成)"
xdotool key F11   # cache_write -> none
sleep 1
xdotool key ctrl+F9
sleep 2
xdotool key ctrl+F8
for _ in $(seq 1 300); do
  gen=$(devtest_value "$log" "T gen"); q=$(devtest_value "$log" "q=")
  if [ "${q:-1}" = "0" ] 2>/dev/null && [ "${gen:-0}" -ge 1615 ] 2>/dev/null; then
    break
  fi
  sleep 0.3
done
rec_gen=$(devtest_value "$log" "T gen"); rec_fail=$(devtest_value "$log" "fail")
rec_inj=$(devtest_value "$log" "T inj")
alive=$(kill -0 $APP_PID 2>/dev/null && echo yes || echo no)
tmp3=$(count_tmp_files "$cache")
echo "recovered: inj=$rec_inj gen=$rec_gen fail=$rec_fail alive=$alive tmp=$tmp3"

kill -TERM "$APP_PID" 2>/dev/null
wait_app_exit 15 || kill_app_hard
screenshot a3-inject-final
echo "DONE"
