#!/usr/bin/env bash
# WP-5 実機E2E検証 (devtoolsビルド + xdotool + Xディスプレイ)。
# 実行: bash docs/dev/measurements/drivers/wp5_e2e.sh
#  (事前に pnpm build (apps/desktop) および
#   cargo build --release -p gento-tauri --features custom-protocol,devtools)
#
# 検証項目 (WP-5受け入れ条件の実E2E証拠):
#   1) 登録ルートで評価 → グリッドバッジ反映 (badge=) → Undoで巻き戻し
#   2) タグ追加 (実キー入力) → 候補表示 (sug=) → 一括適用 (確認表示)
#   3) 高速選別モード (ビューア): 3=評価+自動次送り (V idx増加)・X=除外
#      → セッション集計 (S k=/r=/u= が実DB値を反映)
#   4) 比較表示から除外 (アクティブスロットへXキー)
#   5) キャプション (user_note) 編集 → 手動編集済み表示
#   6) 再起動で評価・タグ・キャプションが復元 (DB正本 + UI表示)
#
# 入力駆動: 実キー入力はxdotool type、UI操作はdev bridgeアクション
# (本物のDOM click → 生産ハンドラ) で行う (qa_wp4_e2e.shと同じ方針)。
# DB検証はpython3 sqlite3 (WAL並行読み取り) で行う。
set -euo pipefail

BIN=/mnt/holyland/Project/GEN-TO/target/release/gento-tauri
WORK=/tmp/opencode/wp5/e2e
DISPLAY="${DISPLAY:-:1}"
export NO_COLOR=1 GENTO_LOG=info GENTO_DEV_BRIDGE=1

rm -rf "$WORK"; mkdir -p "$WORK/env" "$WORK/logs" "$WORK/shots" "$WORK/home"

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

echo "=== 0) 登録ルート追加 → ライブラリ表示 ==="
launch
wait_log "devtest:" 30 || fail "dev bridge not flowing"
# 登録ルートのパス入力へフォーカスして実キー入力 (本物のinput/keydownイベント)
xdotool key ctrl+shift+F2; sleep 0.4
xdotool type --delay 25 "$WORK/home/LIB"; sleep 0.3
xdotool key Return; sleep 1.5
# ハッシュ完了待ち: 全8ファイルがAssetへリンク (裏でBLAKE3計算)
wait_db "SELECT id FROM files WHERE asset_id IS NOT NULL" 8 90 \
  || fail "files not linked to assets (hashing incomplete)"
pass "登録ルート追加 → スキャン+BLAKE3ハッシュ完了 (8ファイル)"
# ツリーでhome直下の一つ目 (=LIB) をクリック → ライブラリ表示へ切替
for _ in $(seq 1 10); do
  xdotool key shift+F7; sleep 2   # openFirstHomeChild (展開→子クリック)
  mode=$(devtest_value mode || true)
  [[ "$mode" == "library" ]] && break
done
[[ "$(devtest_value mode)" == "library" ]] || fail "did not switch to library mode (mode=$(devtest_value mode || echo none))"
[[ "$(devtest_value tot)" == "8" ]] || fail "library catalog should have 8 items (tot=$(devtest_value tot || echo none))"
pass "ライブラリカタログ表示 (mode=library tot=8)"

echo "=== 1) 評価 → グリッドバッジ → Undo ==="
xdotool key ctrl+F2; sleep 0.3          # グリッドフォーカス
xdotool key ctrl+shift+F4; sleep 0.8    # セル0をフォーカス (単一選択)
wait_value edit 1 10 || fail "cell0 not editable (edit=$(devtest_value edit || echo none))"
wait_value rat 0 10 || true
xdotool key ctrl+F4; sleep 1.2          # devアクション: InfoPanel ★5クリック
wait_value rat 5 15 || fail "rating did not apply (rat=$(devtest_value rat || echo none))"
badge=$(devtest_value badge || true)
[[ "$badge" != "0" && -n "$badge" ]] || fail "grid badge not rendered (badge=${badge:-none})"
pass "評価 ★5 → InfoPanel/DB反映 + グリッドバッジ表示 (badge=$badge)"
shot 01-rated-badge
xdotool key ctrl+F5; sleep 1.2          # Undo (★5 → 0へ巻き戻し)
wait_value rat 0 15 || fail "undo did not revert rating (rat=$(devtest_value rat || echo none))"
pass "Ctrl+Z相当のUndoで評価を巻き戻し (rat=0)"
xdotool key ctrl+F4; sleep 1.2          # 再適用 ★5
wait_value rat 5 15 || fail "re-rating failed"

echo "=== 2) タグ追加 → 候補表示 → 一括適用 ==="
xdotool key ctrl+shift+F3; sleep 0.4    # タグ入力へフォーカス
xdotool type --delay 25 -- "E2E Tag"; sleep 0.4
xdotool key Return; sleep 1.2
wait_value tags 1 15 || fail "tag not added (tags=$(devtest_value tags || echo none))"
pass "タグ追加 (実キー入力。tags=1)"
# 候補絞り込み: 入力して200msデバウンス後の候補数 (ポーリング待ち)
xdotool key ctrl+shift+F3; sleep 0.3
xdotool type --delay 25 "e2e"; sleep 0.5
wait_value sug 1 10 || fail "tag suggestion not shown (sug=$(devtest_value sug || echo none))"
pass "タグ候補絞り込み (prefix=e2e → sug=1)"
xdotool key Escape; sleep 0.3
# 一括適用: セル0を選択 → shift+→ で0-1を選択 (実キーイベント) → 除外 (確認→確定)
xdotool key ctrl+F2; sleep 0.3          # グリッドフォーカス
xdotool key ctrl+shift+F4; sleep 0.6    # セル0 (単一選択にリセット)
xdotool key shift+Right; sleep 0.8      # shift+→: セル0-1を範囲選択
[[ "$(devtest_value selN)" == "2" ]] || fail "expected 2 selected (selN=$(devtest_value selN || echo none))"
xdotool key shift+F10; sleep 0.8        # metaReject → 一括確認表示
xdotool key shift+F11; sleep 1.5        # metaBulkConfirm → 適用
tri=$(devtest_value tri || true)
[[ "$tri" == "r" ]] || fail "bulk reject did not apply to focus (tri=${tri:-none})"
wait_db "SELECT id FROM assets WHERE triage = 'rejected'" 2 15 \
  || fail "bulk reject should mark 2 assets in DB (got $(db_query "SELECT id FROM assets WHERE triage = 'rejected'" | wc -l))"
pass "一括適用 (確認表示経由): 2件除外 (focus tri=r + DB 2行)"

echo "=== 3) 高速選別モード (ビューア) → セッション集計 ==="
xdotool key shift+F2; sleep 1.2         # この一覧でセッション開始
wait_value act 1 10 || fail "session did not start (act=$(devtest_value act || echo none))"
xdotool key F3; sleep 1.5               # ビューアで開く (フォーカス=セル1)
xdotool key shift+F12; sleep 0.8        # 高速選別モードON (トグルボタン)
wait_value tmode 1 10 || fail "triage mode not enabled (tmode=$(devtest_value tmode || echo none))"
# セッション開始でフォーカスは位置0 (セル0=一括除外済み) へ移る。
# P (採用) → 3 (評価) → X (除外) の順で未処理セルへ当てる
idx_before=$(devtest_value idx || true)
xdotool key ctrl+shift+F12; sleep 1.0   # "p" = 採用 (セル0: 除外→採用へ) + 自動次送り
wait_db "SELECT id FROM assets WHERE triage = 'picked'" 1 15 \
  || fail "triage-mode pick failed (picked=$(db_query "SELECT id FROM assets WHERE triage = 'picked'" | wc -l))"
idx_after=$(devtest_value idx || echo -1)
[[ "$idx_after" -gt "$idx_before" ]] || fail "auto-advance did not happen (idx $idx_before -> $idx_after)"
pass "高速選別: Pキー=採用適用 + 自動次送り (idx $idx_before→$idx_after)"
xdotool key ctrl+shift+F9; sleep 1.0    # "3" = 評価★3 (セル1) + 自動次送り
wait_db "SELECT id FROM assets WHERE rating = 3" 1 15 \
  || fail "triage-mode rating failed (rating=3 rows: $(db_query "SELECT id FROM assets WHERE rating = 3" | wc -l))"
xdotool key ctrl+shift+F11; sleep 1.0   # "x" = 除外 (セル2 — 未処理)
wait_db "SELECT id FROM assets WHERE triage = 'rejected'" 2 15 \
  || fail "triage-mode reject failed (rejected=$(db_query "SELECT id FROM assets WHERE triage = 'rejected'" | wc -l) want 2)"
pass "高速選別: 3=★3 / X=除外 (DB反映)"
# セッション集計: 採用1 (P) + 除外2 (セル1,2) + 保留5 — DB triageの実集計
sleep 1.5
k=$(devtest_value k || true); r=$(devtest_value r || true)
[[ "$r" == "2" && "$k" == "1" ]] || fail "session counts wrong (k=${k:-none} r=${r:-none} — want k=1 r=2)"
pass "セッション集計がDB実値を反映 (採用1 / 除外2 / 保留5)"
shot 02-triage-session
xdotool key Escape; sleep 0.8           # ビューアを閉じる

echo "=== 4) 比較表示から除外 ==="
# (フォーカスは高速選別後のidx=2 = 採用済み・未除外のassetのまま)
xdotool key F7; sleep 0.5               # cycleChoice: 単一 → 2枚比較
xdotool key F3; sleep 1.8               # 比較表示で開く (スロットはフォーカス位置から充填)
wait_log "C n=2" 10 || fail "compare did not open"
rejected_before=$(db_query "SELECT id FROM assets WHERE triage = 'rejected'" | wc -l)
xdotool key ctrl+shift+F11; sleep 1.2   # X = アクティブスロット (採用済みasset) を除外
wait_db "SELECT id FROM assets WHERE triage = 'rejected'" $((rejected_before + 1)) 15 \
  || fail "compare reject did not persist (still $(db_query "SELECT id FROM assets WHERE triage = 'rejected'" | wc -l))"
pass "比較表示のXキーでアクティブスロットを除外 (DB反映)"
shot 03-compare-cull
xdotool key Escape; sleep 0.8

echo "=== 5) キャプション (user_note) 編集 ==="
xdotool key ctrl+shift+F4; sleep 0.6
xdotool key ctrl+shift+F7; sleep 0.4    # メモtextareaへフォーカス
xdotool type --delay 20 "WP5 E2E memo"; sleep 0.3
xdotool key ctrl+shift+F8; sleep 1.2    # 保存ボタン
caps=$(devtest_value caps || true)
[[ "$caps" == "1" ]] || fail "caption not saved (caps=${caps:-none})"
# python3 sqlite3の行出力はtuple表記 ('...',) のため剥がす
body=$(db_query "SELECT body FROM captions WHERE kind='user_note' AND current=1" | head -1 | sed -E "s/^\('(.*)',?\)$/\1/")
[[ "$body" == "WP5 E2E memo" ]] || fail "caption body mismatch in DB: ${body:-none}"
manu=$(db_query "SELECT manually_edited FROM captions WHERE kind='user_note' AND current=1" | head -1 | tr -d "(), ")
[[ "$manu" == "1" ]] || fail "caption should be manually_edited=1 (got ${manu:-none})"
pass "キャプション編集 (手動編集済み=1。DB body一致)"
shot 04-caption

echo "=== 6) 再起動で復元 ==="
terminate_app
launch
wait_log "devtest:" 30 || fail "dev bridge not flowing after restart"
wait_db "SELECT id FROM files WHERE asset_id IS NOT NULL" 8 60 || true
# ツリーからLIB → ライブラリ表示
for _ in $(seq 1 10); do
  xdotool key shift+F7; sleep 2
  mode=$(devtest_value mode || true)
  [[ "$mode" == "library" ]] && break
done
[[ "$(devtest_value mode)" == "library" ]] || fail "library mode not restored"
xdotool key ctrl+shift+F4; sleep 1.0    # セル0
wait_value rat 5 20 || fail "rating not restored after restart (rat=$(devtest_value rat || echo none))"
caps=$(devtest_value caps || true)
[[ "$caps" == "1" ]] || fail "caption not restored (caps=${caps:-none})"
# タグはセル0に付与済み → 再取得で見える
tags=$(devtest_value tags || true)
[[ "$tags" == "1" ]] || fail "tag not restored (tags=${tags:-none})"
pass "再起動後も評価★5・タグ・キャプションが復元 (rat=5 tags=1 caps=1)"
shot 05-restart-restored
# DB最終状態の整合サマリ
echo "--- DB final state ---"
db_query "SELECT rating, triage, favorite FROM assets ORDER BY id"
terminate_app

echo
echo "WP-5 E2E RESULT: PASS ($pass_count項目)"
echo "artifacts: $WORK (logs + shots)"
