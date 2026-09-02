#!/usr/bin/env bash
# WP-6 実機E2E検証 (devtoolsビルド + xdotool + Xディスプレイ)。
# 実行: bash docs/dev/measurements/drivers/wp6_e2e.sh
#  (事前に pnpm build (apps/desktop) および
#   cargo build --release -p gento-tauri --features custom-protocol,devtools)
#
# 検証項目 (WP-6受け入れ条件の実E2E証拠):
#   1) manual作成 → グリッド選択→コンテキストメニュー→ピッカーで追加
#      (既存へ2件 + 新規コレクションへ1件) → 並び順変更 (先頭へ移動)
#      → コレクション内メモ → 再起動でコレクション・並び順・メモが復元
#   2) smart作成 (評価≥4。既定条件) → WP-5経路で評価を変えたassetが
#      「再評価」で入出する (自動再評価は行われない=明示refresh設計の証明)
#   3) コレクションから選別セッション開始 (origin=コレクション)
#   4) 全操作で元ファイルに変化なし (件数・mtime不変 — fs無操作保証)
#
# 入力駆動: 実キー入力はxdotool type、UI操作はdev bridgeアクション
# (本物のDOM click → 生産ハンドラ) で行う (wp5_e2e.shと同じ方針)。
# DB検証はpython3 sqlite3 (WAL並行読み取り) で行う。
set -euo pipefail

BIN=/mnt/holyland/Project/GEN-TO/target/release/gento-tauri
WORK=/tmp/opencode/wp6/e2e
DISPLAY="${DISPLAY:-:1}"
export NO_COLOR=1 GENTO_LOG=info GENTO_DEV_BRIDGE=1

rm -rf "$WORK"; mkdir -p "$WORK/env" "$WORK/logs" "$WORK/shots" "$WORK/home"

pass_count=0
fail() { echo "[FAIL] $*" >&2; exit 1; }
pass() { pass_count=$((pass_count+1)); echo "[PASS] $*"; }

devtest_value() { grep -a "devtest:" "$WORK/logs/app.log" | tail -1 | grep -oE "\b$1=[-0-9A-Za-z_.]*" | head -1 | cut -d= -f2; }
# CL行のlist=値はカンマを含むため専用抽出
cl_list() { grep -a "devtest:" "$WORK/logs/app.log" | tail -1 | grep -oE "list=[ms0-9,]*" | head -1 | cut -d= -f2; }
wait_cl_list() { # <expected> <timeout_s>
  local deadline=$((SECONDS + ${2:-30})) v=""
  while (( SECONDS < deadline )); do
    v=$(cl_list || true)
    [[ "$v" == "$1" ]] && return 0
    sleep 0.3
  done
  return 1
}
# コレクションコマンドモードに入って数字/記号キーを送る (ctrl+shift+altは
# WM/WebKitへ到達しないための代替チャネル — devtest.ts参照)
coll_key() { # <digit|letter> [wait_s]
  # モーメンタリ: トグル→キー1回でアクション実行+自動退出 (devtest.ts)
  xdotool key ctrl+shift+F1; sleep 0.6
  xdotool key "$1"; sleep "${2:-1.0}"
}
wait_log() { # <pattern> <timeout_s>
  local deadline=$((SECONDS + ${2:-30}))
  while (( SECONDS < deadline )); do
    grep -aqE -- "$1" "$WORK/logs/app.log" && return 0
    sleep 0.2
  done
  return 1
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

APP_PID=""; WID=""
launch() {
  : > "$WORK/logs/app.log"
  HOME="$WORK/home" "$BIN" --dataset "$WORK/home/LIB" --cache-dir "$WORK/env/thumbs" \
    --db "$WORK/env/library.sqlite3" >> "$WORK/logs/app.log" 2>&1 &
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

db_query() { # <sql>
  python3 - "$WORK/env/library.sqlite3" "$1" <<'PY'
import sqlite3, sys
conn = sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True, timeout=5)
try:
    for row in conn.execute(sys.argv[2]):
        print(row)
finally:
    conn.close()
PY
}
wait_db() { # <sql> <expected-rows> <timeout_s>
  local deadline=$((SECONDS + ${3:-30})) n=-1
  while (( SECONDS < deadline )); do
    n=$(db_query "$1" | wc -l)
    [[ "$n" == "$2" ]] && return 0
    sleep 0.4
  done
  return 1
}

# --- フィクスチャ: 登録ルート対象フォルダ (home直下にLIBのみ。8枚) -----------
python3 - "$WORK/home/LIB" <<'PYEOF'
import struct, sys, zlib
from pathlib import Path
root = Path(sys.argv[1]); root.mkdir(parents=True, exist_ok=True)
w = h = 32
def chunk(kind: bytes, data: bytes) -> bytes:
    return (struct.pack(">I", len(data)) + kind + data
            + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF))
for i in range(8):
    raw = b"".join(b"\x00" + bytes([(i * 30) % 256, 150, (row * 5) % 256, 255] * w) for row in range(h))
    png = (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
           + chunk(b"IDAT", zlib.compress(raw, 6)) + chunk(b"IEND", b""))
    (root / f"lib-{i:02}.png").write_bytes(png)
print("fixture LIB: 8 real pngs")
PYEOF

# fs状態のスナップショット (件数+mtime。最後まで不変であることを証明)
fs_snapshot() { find "$WORK/home/LIB" -type f -printf "%f %T@\n" | sort; }
fs_snapshot > "$WORK/env/fs-before.txt"

echo "=== 0) 登録ルート追加 → ライブラリ表示 ==="
launch
wait_log "devtest:" 30 || fail "dev bridge not flowing"
xdotool key ctrl+shift+F2; sleep 0.4
xdotool type --delay 25 "$WORK/home/LIB"; sleep 0.3
xdotool key Return; sleep 1.5
wait_db "SELECT id FROM files WHERE asset_id IS NOT NULL" 8 90 \
  || fail "files not linked to assets (hashing incomplete)"
pass "登録ルート追加 → スキャン+BLAKE3ハッシュ完了 (8ファイル)"
for _ in $(seq 1 10); do
  xdotool key shift+F7; sleep 2
  mode=$(devtest_value mode || true)
  [[ "$mode" == "library" ]] && break
done
[[ "$(devtest_value mode)" == "library" ]] || fail "did not switch to library mode"
[[ "$(devtest_value tot)" == "8" ]] || fail "library catalog should have 8 items"
pass "ライブラリカタログ表示 (mode=library tot=8)"

echo "=== 1) manual作成 → 追加 (既存へ2件 + 新規へ1件) ==="
coll_key 1 0.8                                # 「+ 手動」フォーム + 名前フォーカス
xdotool type --delay 25 "E2E Manual"; sleep 0.3
xdotool key Return; sleep 1.2
wait_value n 1 15 || fail "collection not created (CL n=$(devtest_value n || echo none))"
[[ "$(devtest_value list)" == "m0" ]] || fail "unexpected list (list=$(devtest_value list || echo none))"
pass "manualコレクション作成 (CL n=1)"

# セル0-1を選択 (実キー) → コンテキストメニュー → ピッカーで既存へ追加
xdotool key ctrl+F2; sleep 0.3
xdotool key ctrl+shift+F4; sleep 0.5
xdotool key shift+Right; sleep 0.8
[[ "$(devtest_value selN)" == "2" ]] || fail "expected 2 selected (selN=$(devtest_value selN || echo none))"
coll_key 4 0.8                                # cell0コンテキストメニュー
coll_key 5 1.0                                # 「コレクションへ追加…」→ ピッカー
coll_key 6 1.4                                # 既存「E2E Manual」へ追加
xdotool key Escape; sleep 0.6                  # ピッカーを閉じる
wait_value list m2 15 || fail "items not added (CL list=$(devtest_value list || echo none))"
wait_db "SELECT asset_id FROM collection_items" 2 15 || fail "collection_items rows != 2"
pass "選択2件を既存コレクションへ追加 (コンテキストメニュー→ピッカー。list=m2)"

# セル2 (単一) → 新規コレクション「Second」を作成して追加
xdotool key ctrl+shift+F4; sleep 0.4
xdotool key Right; sleep 0.6                  # セル1→2へフォーカス移動 (単一選択)
xdotool key ctrl+shift+alt+F5; sleep 0.8
coll_key 4 0.8
coll_key 5 1.0
coll_key 7 0.6                                # 新規名入力へフォーカス
xdotool type --delay 25 "Second"; sleep 0.3
xdotool key Return; sleep 1.5
xdotool key Escape; sleep 0.6                  # ピッカーを閉じる
wait_value n 2 15 || fail "second collection not created (n=$(devtest_value n || echo none))"
wait_cl_list m2,m1 10 || fail "unexpected list (list=$(cl_list || echo none))"
wait_db "SELECT id FROM collections WHERE name='Second'" 1 10 || fail "Second not in DB"
pass "新規コレクション「Second」を作成して1件追加 (CL n=2)"

echo "=== 2) manual表示 → 並び順変更 → メモ ==="
coll_key 3 1.5                                # 次のコレクション = E2E Manualを表示
[[ "$(devtest_value kind)" == "m" ]] || fail "expected manual display (kind=$(devtest_value kind || echo none))"
[[ "$(devtest_value tot)" == "2" ]] || fail "manual view should show 2 (tot=$(devtest_value tot || echo none))"
[[ "$(devtest_value exp)" == "1" ]] || fail "export entry (disabled) not shown"
pass "manualコレクション表示 (position順。tot=2) + エクスポート入口表示"
shot 01-manual-view
# 並び順: セル1 (2番目) を先頭へ
positions_before=$(db_query "SELECT asset_id, position FROM collection_items WHERE collection_id=(SELECT id FROM collections WHERE name='E2E Manual') ORDER BY position, asset_id")
xdotool key ctrl+F2; sleep 0.3
xdotool key ctrl+shift+F4; sleep 0.4          # セル0 (単一選択)
xdotool key Right; sleep 0.5                  # セル1へ
coll_key 8 1.5                                # 選択を先頭へ
positions_after=$(db_query "SELECT asset_id, position FROM collection_items WHERE collection_id=(SELECT id FROM collections WHERE name='E2E Manual') ORDER BY position, asset_id")
[[ "$positions_before" != "$positions_after" ]] || fail "reorder did not change positions"
db_query "SELECT position FROM collection_items WHERE collection_id=(SELECT id FROM collections WHERE name='E2E Manual') ORDER BY position" | tr -d '(),' | tr '\n' ' ' | grep -q "^0 1 $" \
  || fail "positions not contiguous after reorder ($(db_query "SELECT position FROM collection_items WHERE collection_id=(SELECT id FROM collections WHERE name='E2E Manual') ORDER BY position" | tr '\n' ' '))"
pass "並び順変更 (選択を先頭へ。position=0,1の連番維持)"
shot 02-reordered
# メモ: フォーカス (先頭=移動済み項目) のコレクション内メモ
# (reorderでgenerationが進みpager再取得中はfocusInfoがnullになるため
#  edit=1 (フォーカス項目が編集可能=Asset確定) を待ってから進む)
xdotool key ctrl+shift+F4; sleep 0.8
wait_value edit 1 20 || fail "focus item not ready after reorder (edit=$(devtest_value edit || echo none))"
coll_key m 1.0                                # メモtextareaフォーカス
xdotool type --delay 20 "WP6 E2E memo"; sleep 0.3
coll_key s 1.2                                # 保存
note=$(db_query "SELECT note FROM collection_items WHERE note IS NOT NULL" | head -1 | sed -E "s/^\('(.*)',?\)$/\1/")
[[ "$note" == "WP6 E2E memo" ]] || fail "item note mismatch in DB: ${note:-none}"
pass "コレクション内メモ保存 (DB note一致)"

echo "=== 3) smart作成 (評価≥4) → 明示refreshで入出 ==="
# 一旦ライブラリ表示へ戻す (ツリー選択) — 評価付けのため
for _ in $(seq 1 10); do
  xdotool key shift+F7; sleep 2
  [[ "$(devtest_value exp)" == "0" ]] && break
done
xdotool key ctrl+shift+F4; sleep 0.6          # セル0 (asset A)
xdotool key ctrl+F4; sleep 1.2                # InfoPanel ★5 → A rating=5
wait_db "SELECT id FROM assets WHERE rating >= 4" 1 15 || fail "rating did not apply"
coll_key 2 0.8                                # 「+ スマート」フォーム (既定=評価≥4)
xdotool type --delay 25 "High Rated"; sleep 0.3
xdotool key Return; sleep 1.2
wait_value n 3 15 || fail "smart not created (n=$(devtest_value n || echo none))"
wait_cl_list m2,m1,s1 10 || fail "unexpected list (list=$(cl_list || echo none))"
qj=$(db_query "SELECT query_json FROM collections WHERE kind='smart'" | head -1)
# QA-WP8以降、smartコレクションのquery_jsonはv2 AST形式
# (terms[].{key,op,value}。旧トークンrating_geは廃止)
echo "$qj" | grep -q '"key":"rating","op":"ge"' || fail "query_json missing rating ge term: $qj"
echo "$qj" | grep -q 'SELECT\|FROM assets' && fail "query_json must not contain SQL"
pass "smartコレクション作成 (評価≥4。query_jsonのみでSQL不保存)"
# 表示 (E2E Manual→Second→High Rated の3巡回でsmartへ到達)
coll_key 3 1.0
coll_key 3 1.0
coll_key 3 1.5
[[ "$(devtest_value kind)" == "s" ]] || fail "expected smart display (kind=$(devtest_value kind || echo none))"
[[ "$(devtest_value tot)" == "1" ]] || fail "smart view should show 1 (tot=$(devtest_value tot || echo none))"
pass "smartコレクション表示 (評価結果。tot=1)"
# メンバーの評価を外す (★5トグル→0。WP-5経路) — 表示は自動再評価しない
xdotool key ctrl+shift+F4; sleep 0.5
xdotool key ctrl+F4; sleep 1.2                # ★5再クリック = 評価解除 (トグル)
wait_db "SELECT id FROM assets WHERE rating >= 4" 0 15 || fail "rating toggle did not persist"
sleep 1.0
[[ "$(devtest_value tot)" == "1" ]] || fail "smart view must NOT auto re-evaluate (tot=$(devtest_value tot || echo none) — 明示refresh設計違反)"
pass "メタデータ変更で自動再評価しない (明示refresh設計)"
# 明示「再評価」→ メンバーが抜ける
coll_key 0 1.5                                # 再評価
wait_value tot 0 15 || fail "refresh did not remove unrated member (tot=$(devtest_value tot || echo none))"
[[ "$(devtest_value cnt)" == "0" ]] || fail "smart count after refresh (cnt=$(devtest_value cnt || echo none))"
pass "明示「再評価」でメンバー退出 (tot=1→0)"
# 再度 ★5 (smart表示は空になったためライブラリへ戻してから) → 次の表示時評価で戻る
for _ in $(seq 1 10); do
  xdotool key shift+F7; sleep 2
  [[ "$(devtest_value exp)" == "0" ]] && break
done
[[ "$(devtest_value exp)" == "0" ]] || fail "did not return to library view"
xdotool key ctrl+shift+F4; sleep 0.8
wait_value edit 1 15 || true
xdotool key ctrl+F4; sleep 1.2
wait_db "SELECT id FROM assets WHERE rating >= 4" 1 15 || fail "re-rating failed"
coll_key 3 0.9                                # E2E Manual
coll_key 3 0.9                                # Second
coll_key 3 1.5                                # High Rated (再表示=評価)
[[ "$(devtest_value kind)" == "s" ]] || fail "expected smart re-display"
wait_value tot 1 15 || fail "re-show should re-evaluate (tot=$(devtest_value tot || echo none))"
pass "再表示時評価でメンバー復帰 (tot=0→1)"
shot 03-smart-refresh

echo "=== 4) コレクションから選別セッション開始 ==="
# E2E Manualを表示 (smart→E2E Manual へ1巡回)
coll_key 3 1.5
[[ "$(devtest_value kind)" == "m" ]] || fail "expected manual display for session (kind=$(devtest_value kind || echo none))"
[[ "$(devtest_value tot)" == "2" ]] || fail "manual view should show 2 for session (tot=$(devtest_value tot || echo none))"
xdotool key shift+F2; sleep 1.5               # 現在の一覧でセッション開始
wait_value act 1 10 || fail "session did not start (act=$(devtest_value act || echo none))"
[[ "$(devtest_value tot)" == "2" ]] || fail "session over collection should have 2 ids (tot=$(devtest_value tot || echo none))"
pass "コレクションから選別セッション開始 (origin=コレクション。act=1 tot=2)"
shot 04-session-from-collection

echo "=== 5) 再起動 → コレクション・並び順・メモ・セッション復元 ==="
terminate_app
launch
wait_log "devtest:" 30 || fail "dev bridge not flowing after restart"
wait_db "SELECT id FROM files WHERE asset_id IS NOT NULL" 8 60 || true
for _ in $(seq 1 10); do
  xdotool key shift+F7; sleep 2
  [[ "$(devtest_value mode)" == "library" ]] && break
done
[[ "$(devtest_value mode)" == "library" ]] || fail "library mode not restored"
wait_value n 3 20 || fail "collections not restored (n=$(devtest_value n || echo none))"
wait_cl_list m2,m1,s1 10 || fail "collection list mismatch after restart (list=$(cl_list || echo none))"
note=$(db_query "SELECT note FROM collection_items WHERE note IS NOT NULL" | head -1 | sed -E "s/^\('(.*)',?\)$/\1/")
[[ "$note" == "WP6 E2E memo" ]] || fail "note lost after restart: ${note:-none}"
positions_restart=$(db_query "SELECT asset_id, position FROM collection_items WHERE collection_id=(SELECT id FROM collections WHERE name='E2E Manual') ORDER BY position, asset_id")
[[ "$positions_restart" == "$positions_after" ]] || fail "positions changed across restart ($positions_restart vs $positions_after)"
pass "再起動でコレクション3件・並び順・メモが復元 (list=m2,m1,s1)"
# セッション再開 (origin=コレクション → コレクション再表示してID解決)
xdotool key shift+F3; sleep 2.5               # sessionResume
wait_value act 1 15 || fail "collection session did not resume (act=$(devtest_value act || echo none))"
[[ "$(devtest_value kind)" == "m" ]] || fail "resume should re-show the collection (kind=$(devtest_value kind || echo none))"
[[ "$(devtest_value tot)" == "2" ]] || fail "resumed session ids (tot=$(devtest_value tot || echo none))"
pass "コレクション由来セッションの再開 (コレクション再表示+ID解決)"
shot 05-restart-resumed

echo "=== 6) fs無操作保証 (件数・mtime不変) ==="
fs_snapshot > "$WORK/env/fs-after.txt"
if diff -u "$WORK/env/fs-before.txt" "$WORK/env/fs-after.txt" > "$WORK/env/fs-diff.txt"; then
  pass "全コレクション操作で元ファイル無変化 (8ファイル・mtime完全一致)"
else
  cat "$WORK/env/fs-diff.txt" >&2
  fail "filesystem changed during collection operations"
fi

echo "--- DB final state ---"
db_query "SELECT id, name, kind FROM collections ORDER BY id"
db_query "SELECT collection_id, asset_id, position, note FROM collection_items ORDER BY collection_id, position"
terminate_app

echo
echo "WP-6 E2E RESULT: PASS ($pass_count項目)"
echo "artifacts: $WORK (logs + shots)"
