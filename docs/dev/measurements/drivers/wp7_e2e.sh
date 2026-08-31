#!/usr/bin/env bash
# WP-7 (MVP) 実機E2E検証 (devtoolsビルド + xdotool + Xディスプレイ)。
# 実行: bash docs/dev/measurements/drivers/wp7_e2e.sh
#  (事前に pnpm build (apps/desktop) および
#   cargo build --release -p gento-tauri --features custom-protocol,devtools)
#
# 検証項目 (WP-7受け入れ条件の実E2E証拠。安全性テストの実機側):
#   1) コピー→同名衝突→両方保持 → DB二重登録なし (files=+1, assets不変=同一hash統合)
#   2) 移動 (サブフォルダへ) → DB rel_path更新・同一行id・os_file_id不変・
#      watcher二重反映なし (移動後も行が1行のみ)
#   3) リネーム (コンテキストメニュー→F2インライン入力) → FS+DB一致
#   4) ゴミ箱 (Delete) → missing化 + OSゴミ箱 (~/.local/share/Trash/files) 実在
#   5) Undo混在履歴: ゴミ箱→リネーム→評価 (Ctrl+Zで逆順に巻き戻し)
#      + 外部変更後のリネームUndo拒否 (ファイルが改変されても無傷)
#   5b) 上書き (rename→既存名。FG-WP7確認トークン新フロー): 旧ファイルは
#       ゴミ箱へ退避保護 → Undoで旧内容まで復元
#   6) 完全削除 (Shift+Delete + チェックボックス確認) → ゴミ箱を経由しない
#   7) 再起動でmissing維持 (再スキャンでも復活しない)
#
# 入力駆動: 実キー入力 (Delete/F2入力/Ctrl+Z) はxdotool、UI操作 (コンテキスト
# メニュー・宛先/衝突/完全削除ダイアログ) はdev bridgeアクション
# (本物のDOM click → 生産ハンドラ) で行う (wp6_e2e.shと同じ方針)。
# DB検証はpython3 sqlite3 (WAL並行読み取り) で行う。
set -euo pipefail

BIN=/mnt/holyland/Project/GEN-TO/target/release/gento-tauri
WORK=/tmp/opencode/wp7/e2e
# 共有デスクトップ (:1) が他セッションに占有されているとフォーカス不安定に
# なるため、専用のネストXディスプレイ (:5 = Xephyr) で実行する。
# Xephyrが無ければ起動する (WM無しでも windowfocus+クリックでキー注入可)。
# 明示指定は GENTO_E2E_DISPLAY (呼び出し元のDISPLAY環境変数は無視する —
# セッション共有の :1 が偶発的に使われてフォーカスを失うのを防ぐ)。
DISPLAY="${GENTO_E2E_DISPLAY:-:5}"
export NO_COLOR=1 GENTO_LOG=info GENTO_DEV_BRIDGE=1
if ! xdpyinfo -display "$DISPLAY" >/dev/null 2>&1; then
  setsid nohup Xephyr "$DISPLAY" -screen 1600x1000 >/tmp/opencode/wp7-xephyr.log 2>&1 </dev/null &
  for _ in $(seq 1 50); do
    xdpyinfo -display "$DISPLAY" >/dev/null 2>&1 && break
    sleep 0.2
  done
fi
xdpyinfo -display "$DISPLAY" >/dev/null 2>&1 || { echo "no X display on $DISPLAY" >&2; exit 1; }

rm -rf "$WORK"; mkdir -p "$WORK/env" "$WORK/logs" "$WORK/shots" "$WORK/home"

pass_count=0
fail() { echo "[FAIL] $*" >&2; exit 1; }
pass() { pass_count=$((pass_count+1)); echo "[PASS] $*"; }

devtest_line() { grep -a "devtest:" "$WORK/logs/app.log" | tail -1; }
devtest_value() { devtest_line | grep -oE "\b$1=[-0-9A-Za-z_.]*" | head -1 | cut -d= -f2; }
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
# FO行の特定フィールドが "1" になるのを待つ (ダイアログ表示など)
wait_fo() { wait_value "$1" 1 "${2:-15}"; }

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
  # WebViewの起動完了を待つ (キー注入の着地要素が揃ってから対話する)
  wait_log "devtest:" 30 || fail "webview not ready"
  sleep 2
  # WMの無いXephyr上では明示的なフォーカス設定+グリッド上のクリックが
  # 必要 (GTKはボタン押下でキーボードフォーカスを取る。以降の各所で
  # 選択は明示的に設定し直すため、起動時のセル選択は無害)。
  xdotool windowfocus "$WID" || true
  xdotool mousemove 400 300 click 1 || true
  sleep 1
}
terminate_app() {
  [[ -n "${APP_PID:-}" ]] && kill -TERM "$APP_PID" 2>/dev/null || true
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
# present表示順 (カタログと同一の名前順) の cell<index> の表示名
# present表示順 (カタログ=source_path順と同一。サブフォルダ行は後ろへ並ぶ —
# build_library_snapshotのsortと同じ意味論) の cell<index> の表示名
db_present_name() { # <index>
  db_query "SELECT display_name FROM files WHERE scan_state='present' ORDER BY rel_path"     | sed -n "$(($1 + 1))p" | sed -E "s/^\('(.*)',?\)$/\1/"
}

# ファイル操作コマンドモード (ctrl+shift+F12 → 1キーでアクション+自動退出)
fo_key() { # <key> [wait_s]
  xdotool key ctrl+shift+F12; sleep 0.6
  xdotool key "$1"; sleep "${2:-1.0}"
}

# --- フィクスチャ: 登録ルート対象フォルダ (8枚 + 移動先用サブフォルダ) --------
python3 - "$WORK/home/LIB" <<'PYEOF'
import struct, sys, zlib
from pathlib import Path
root = Path(sys.argv[1]); (root / "sub").mkdir(parents=True, exist_ok=True)
w = h = 32
def chunk(kind: bytes, data: bytes) -> bytes:
    return (struct.pack(">I", len(data)) + kind + data
            + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF))
for i in range(8):
    raw = b"".join(b"\x00" + bytes([(i * 30) % 256, 150, (row * 5) % 256, 255] * w) for row in range(h))
    png = (b"\89PNG\r\n\x1a\n".replace(b"\89", b"\x89") + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
           + chunk(b"IDAT", zlib.compress(raw, 6)) + chunk(b"IEND", b""))
    (root / f"lib-{i:02}.png").write_bytes(png)
print("fixture LIB: 8 real pngs + sub/")
PYEOF

echo "=== 0) 起動 → 登録ルート → ライブラリ表示 ==="
launch
wait_log "devtest:" 30 || fail "dev bridge not flowing"
# 登録ルート追加: 共有デスクトップでxdotool typeが不安定なため
# dev bridge経由でフォームに--datasetパスを設定して追加 (生産経路)
fo_key 0 2.0
wait_db "SELECT id FROM files WHERE asset_id IS NOT NULL" 8 90 \
  || fail "files not linked to assets (hashing incomplete)"
pass "登録ルート追加 → スキャン+BLAKE3完了 (8ファイル)"
for _ in $(seq 1 10); do
  xdotool key shift+F7; sleep 2
  mode=$(devtest_value mode || true)
  [[ "$mode" == "library" ]] && break
done
[[ "$(devtest_value mode)" == "library" ]] || fail "did not switch to library mode"
[[ "$(devtest_value tot)" == "8" ]] || fail "library catalog should have 8 items"
pass "ライブラリカタログ表示 (mode=library tot=8)"

echo "=== 1) リネーム (メニュー→インライン入力。他操作より先に行う — リフレッシュ後のグリッド表示揺らぎを避ける) ==="
[[ "$(db_present_name 2)" == "lib-02.png" ]] || fail "cell2 should be lib-02.png ($(db_present_name 2))"
xdotool key ctrl+shift+F6; sleep 0.8           # セル2 (lib-02.png) を単一選択
fo_key 5 0.8                                  # フォーカスセルのコンテキストメニュー
fo_key e 1.2                                  # 「リネーム (F2)」→ インライン入力
wait_fo rn 10 || fail "rename input not shown (rn=$(devtest_value rn || echo -))"
shot 03-rename-inline
# "will-revert.png"設定+確定 (生産経路)。入力フォーカス揺らぎで稀にキーが
# 落ちるため、確定まで有限回再試行する (wp6 run14と同種の揺らぎ)
for _ in $(seq 1 5); do
  [[ "$(devtest_value rn || echo 0)" == "1" ]] || break
  fo_key w 2.0
done
wait_db "SELECT id FROM files WHERE CAST(rel_path AS TEXT)='will-revert.png'" 1 20 \
  || fail "rename did not update DB (rn=$(devtest_value rn || echo -))"
[[ ! -f "$WORK/home/LIB/lib-02.png" ]] || fail "old name still exists"
[[ -f "$WORK/home/LIB/will-revert.png" ]] || fail "renamed file missing"
wait_value undo 1 15 || fail "undo=1 expected after rename (undo=$(devtest_value undo || echo -))"
pass "リネーム: FS・DBのrel_path/display_name一致 (will-revert.png) + undo=1"

echo "=== 2) コピー → 同名衝突 → 両方保持 → Asset統合 (二重登録なし) ==="
xdotool key ctrl+shift+F4; sleep 0.5          # セル0 (lib-00.png)
[[ "$(db_present_name 0)" == "lib-00.png" ]] || fail "cell0 should be lib-00.png ($(db_present_name 0))"
fo_key 4 0.8                                  # cell0コンテキストメニュー
fo_key c 0.8                                  # 「コピー…」→ 宛先ダイアログ
wait_fo dst 10 || fail "dest picker not shown (dst=$(devtest_value dst || echo -))"
shot 01-dest-picker
fo_key d 1.5                                  # 最初のノード (=LIB自体) へコピー
wait_fo cf 10 || fail "conflict dialog not shown (cf=$(devtest_value cf || echo -))"
shot 02-conflict
fo_key k 2.5                                  # 両方保持
wait_db "SELECT id FROM files WHERE CAST(rel_path AS TEXT)='lib-00 (2).png' AND asset_id IS NOT NULL" 1 30 \
  || fail "keep_both copy not in DB"
[[ -f "$WORK/home/LIB/lib-00 (2).png" ]] || fail "keep_both file missing on disk"
[[ "$(db_query "SELECT count(*) FROM files" | head -1 | tr -dc '0-9')" == "9" ]] \
  || fail "files should be 9 after copy"
assets_n=$(db_query "SELECT count(*) FROM assets" | head -1 | tr -dc '0-9')
[[ "$assets_n" == "8" ]] || fail "assets must stay 8 (同一hash統合。got $assets_n)"
same_asset=$(db_query "SELECT count(DISTINCT asset_id) FROM files WHERE CAST(rel_path AS TEXT) IN ('lib-00.png','lib-00 (2).png')" | head -1 | tr -dc '0-9')
[[ "$same_asset" == "1" ]] || fail "copy should share the asset (hash統合)"
wait_value undo 2 15 || fail "undo entry not pushed (undo=$(devtest_value undo || echo -))"
pass "コピー衝突→両方保持: files=9 / assets=8 (同一hash統合) / undo=2"

echo "=== 3) 移動 (sub/へ) → rel_path更新・同一行・os_file_id不変・watcher単一反映 ==="
# 移動対象: lib-01.png (カタログ順: 00(2), 00, 01, 03, ... → セル2)
[[ "$(db_present_name 2)" == "lib-01.png" ]] || fail "cell2 should be lib-01.png ($(db_present_name 2))"
before=$(db_query "SELECT id, uuid, os_file_id, asset_id FROM files WHERE CAST(rel_path AS TEXT)='lib-01.png'")
xdotool key ctrl+shift+F6; sleep 0.8           # セル2 (lib-01.png) を単一選択
fo_key 5 0.8                                  # フォーカスセルのコンテキストメニュー
fo_key v 0.8                                  # 「移動…」
wait_fo dst 10 || fail "dest picker not shown for move"
fo_key b 4.0                                  # 展開→最初の子 (=sub) へ移動
wait_db "SELECT id FROM files WHERE CAST(rel_path AS TEXT)='sub/lib-01.png'" 1 30 \
  || fail "move did not update DB rel_path"
[[ ! -f "$WORK/home/LIB/lib-01.png" ]] || fail "source file still exists after move"
[[ -f "$WORK/home/LIB/sub/lib-01.png" ]] || fail "moved file missing"
after=$(db_query "SELECT id, uuid, os_file_id, asset_id FROM files WHERE CAST(rel_path AS TEXT)='sub/lib-01.png'")
[[ "$before" == "$after" ]] || fail "row identity changed by move:
  before=$before
  after =$after"
# watcher二重反映なし: 3秒待って同一行が1行のみ・総数不変
sleep 3
[[ "$(db_query "SELECT count(*) FROM files WHERE CAST(rel_path AS TEXT) LIKE '%lib-01%'" | head -1 | tr -dc '0-9')" == "1" ]] \
  || fail "watcher double-apply: duplicated rows for lib-01"
[[ "$(db_query "SELECT count(*) FROM files" | head -1 | tr -dc '0-9')" == "9" ]] \
  || fail "row count changed after move (二重反映の疑い)"
pass "移動: 同一行id/uuid/os_file_id/asset維持・rel_path=sub/更新・watcher単一反映"

echo "=== 4) 外部変更後のUndo拒否 (移動巻き戻し拒否) ==="
# sub/lib-01.png を外部から改変 → Ctrl+Z (直近=移動) は前提崩れで拒否
python3 - "$WORK/home/LIB/sub/lib-01.png" <<'PY'
import sys
from pathlib import Path
p = Path(sys.argv[1])
p.write_bytes(p.read_bytes() + b"EXTERNAL-MODIFICATION")
print("externally modified")
PY
xdotool key ctrl+z; sleep 2.5                  # Ctrl+Z (実キー) → 拒否
[[ -f "$WORK/home/LIB/sub/lib-01.png" ]] \
  || fail "undo refusal must keep the moved file in place"
grep -aq "EXTERNAL-MODIFICATION" "$WORK/home/LIB/sub/lib-01.png" \
  || fail "externally modified content must be intact after refused undo"
wait_db "SELECT id FROM files WHERE CAST(rel_path AS TEXT)='sub/lib-01.png'" 1 10 \
  || fail "DB must be unchanged after refused undo"
wait_value undo 2 10 || fail "failed undo entry popped (undo=$(devtest_value undo || echo -))"
pass "外部変更後のUndo拒否: ファイル・DB無傷 (前提検証の実機証明)"

echo "=== 5) Undo: コピー削除 → リネーム巻き戻し (混在履歴を逆順) ==="
xdotool key ctrl+z; sleep 2.5                  # コピーのUndo = 生成物削除 (ゴミ箱へ)
[[ ! -f "$WORK/home/LIB/lib-00 (2).png" ]] || fail "copy-undo left the generated file"
[[ -f "$WORK/home/LIB/lib-00.png" ]] || fail "copy-undo must not touch the original"
wait_db "SELECT id FROM files WHERE CAST(rel_path AS TEXT)='lib-00 (2).png' AND scan_state='missing'" 1 15 \
  || fail "copy row should be missing after undo"
xdotool key ctrl+z; sleep 2.5                  # リネームのUndo = lib-02.pngへ戻る
[[ -f "$WORK/home/LIB/lib-02.png" ]] || fail "rename-undo did not restore the name"
[[ ! -f "$WORK/home/LIB/will-revert.png" ]] || fail "old name still exists after rename-undo"
wait_db "SELECT id FROM files WHERE CAST(rel_path AS TEXT)='lib-02.png'" 1 15 \
  || fail "rename-undo did not update DB"
pass "Undo (コピー生成物削除→リネーム巻き戻し): FS・DB整合"

echo "=== 5b) 上書き (rename→既存名。FG-WP7確認トークン新フロー) → 旧ファイル退避保護 → Undo旧内容復元 ==="
cp "$WORK/home/LIB/lib-04.png" "$WORK/lib-04-new.png"
cp "$WORK/home/LIB/lib-05.png" "$WORK/lib-05-old.png"
fo_key 3 0.8                                  # セル3 (lib-04.png) を選択
[[ "$(db_present_name 3)" == "lib-04.png" ]] || fail "cell3 should be lib-04.png ($(db_present_name 3))"
fo_key 5 0.8                                  # フォーカスセルのコンテキストメニュー
fo_key e 1.2                                  # 「リネーム (F2)」→ インライン入力
wait_fo rn 10 || fail "rename input not shown for overwrite (rn=$(devtest_value rn || echo -))"
fo_key q 2.5                                  # "lib-05.png" (既存名) へ確定 → 衝突
wait_fo cf 15 || fail "conflict dialog not shown for overwrite rename (cf=$(devtest_value cf || echo -))"
shot 05-overwrite-conflict
fo_key o 4.0                                  # 上書き (計画トークン付き実行)
# FS: lib-05.pngは新内容 (lib-04の実体)・lib-04.pngは消える
[[ ! -f "$WORK/home/LIB/lib-04.png" ]] || fail "rename-overwrite should remove lib-04.png"
cmp -s "$WORK/home/LIB/lib-05.png" "$WORK/lib-04-new.png" \
  || fail "overwritten lib-05.png must carry the NEW content"
# 旧ファイル (lib-05の旧実体) はOSゴミ箱へ退避されている (旧ファイル保護)
trash_old=$(ls "$WORK/home/.local/share/Trash/files/" 2>/dev/null | grep -E '^lib-05\.png(\.[0-9]+)?$' | head -1)
[[ -n "$trash_old" ]] || fail "old lib-05.png must be evacuated to OS trash"
cmp -s "$WORK/home/.local/share/Trash/files/$trash_old" "$WORK/lib-05-old.png" \
  || fail "evacuated old file must carry the OLD content"
# Undo → 新実体はlib-04へ戻り・旧内容がlib-05へ復元される (複合undo)
xdotool key ctrl+z; sleep 3
[[ -f "$WORK/home/LIB/lib-04.png" ]] || fail "undo must restore lib-04.png"
cmp -s "$WORK/home/LIB/lib-04.png" "$WORK/lib-04-new.png" \
  || fail "lib-04.png must carry the moved-back new content"
cmp -s "$WORK/home/LIB/lib-05.png" "$WORK/lib-05-old.png" \
  || fail "undo must restore the OLD content byte-for-byte"
wait_db "SELECT id FROM files WHERE CAST(rel_path AS TEXT)='lib-05.png' AND scan_state='present'" 1 15 \
  || fail "restored old file row not present"
pass "上書き: 旧ファイルゴミ箱退避保護 + Undo旧内容復元 (確認トークン新フロー)"

echo "=== 6) ゴミ箱 (Delete) → missing化 + OSゴミ箱実在 → Undo復帰 ==="
target=$(db_present_name 0)                    # 表示順先頭
xdotool key ctrl+shift+F4; sleep 0.5
xdotool key Delete; sleep 2.5                  # Delete (実キー。生産経路)
[[ ! -f "$WORK/home/LIB/$target" ]] || fail "target not removed from LIB: $target"
wait_db "SELECT id FROM files WHERE CAST(rel_path AS TEXT)='$target' AND scan_state='missing'" 1 20 \
  || fail "trashed row not marked missing"
[[ -f "$WORK/home/.local/share/Trash/files/$target" ]] \
  || fail "OS trash does not contain $target"
wait_value undo 1 15 || fail "undo=1 expected after trash (undo=$(devtest_value undo || echo -))"
pass "ゴミ箱: missing化 + ~/.local/share/Trash/files実在 + undo=1"
xdotool key ctrl+z; sleep 2.5                  # ゴミ箱Undo = OSゴミ箱から復帰
[[ -f "$WORK/home/LIB/$target" ]] || fail "trash undo did not restore file"
wait_db "SELECT id FROM files WHERE CAST(rel_path AS TEXT)='$target' AND scan_state='present'" 1 20 \
  || fail "restored row not present"
[[ ! -f "$WORK/home/.local/share/Trash/files/$target" ]] \
  || fail "trash entry should be consumed by restore"
pass "ゴミ箱Undo: OSゴミ箱から元位置へ復帰・DB present復帰"

echo "=== 7) メタデータ混在Undo (評価) ==="
xdotool key ctrl+shift+F4; sleep 0.5
xdotool key ctrl+F4; sleep 1.5                 # InfoPanel ★5
wait_db "SELECT id FROM assets WHERE rating >= 4" 1 15 || fail "rating did not apply"
wait_value undo 1 15 || true
xdotool key ctrl+z; sleep 2.0                  # 評価のUndo (メタデータ)
wait_db "SELECT id FROM assets WHERE rating >= 4" 0 15 || fail "rating undo failed"
pass "混在Undo: 評価 (メタデータ) もファイル操作と同一スタックで巻き戻し"

echo "=== 8) 完全削除 (Shift+Delete + 確認チェックボックス) ==="
target2=$(db_present_name 0)
xdotool key ctrl+shift+F4; sleep 0.5
xdotool key shift+Delete; sleep 1.5            # Shift+Delete (実キー)
wait_fo fv 10 || fail "forever dialog not shown (fv=$(devtest_value fv || echo -))"
shot 04-forever-dialog
fo_key f 0.6                                   # 「完全に削除する」チェック
fo_key g 2.5                                   # 確定
[[ ! -f "$WORK/home/LIB/$target2" ]] || fail "forever delete left the file: $target2"
[[ ! -f "$WORK/home/.local/share/Trash/files/$target2" ]] \
  || fail "forever delete must NOT go through the OS trash"
wait_db "SELECT id FROM files WHERE CAST(rel_path AS TEXT)='$target2' AND scan_state='missing'" 1 20 \
  || fail "forever-deleted row not missing"
pass "完全削除: ゴミ箱非経由の消去 + missing化 (確認2段: チェック+ボタン)"

echo "=== 9) 再起動でmissing維持 (再スキャンで復活しない) ==="
present_before=$(db_query "SELECT count(*) FROM files WHERE scan_state='present'" | head -1 | tr -dc '0-9')
terminate_app
launch
wait_log "devtest:" 30 || fail "dev bridge not flowing (restart)"
sleep 6
missing_n=$(db_query "SELECT count(*) FROM files WHERE scan_state='missing'" | head -1 | tr -dc '0-9')
[[ "$missing_n" == "2" ]] || fail "missing rows must stay 2 after restart (got $missing_n)"
present_after=$(db_query "SELECT count(*) FROM files WHERE scan_state='present'" | head -1 | tr -dc '0-9')
[[ "$present_before" == "$present_after" ]] \
  || fail "present count changed across restart ($present_before -> $present_after)"
pass "再起動: missing維持 (欠損2行が再スキャンで復活しない)"

echo
echo "WP-7 E2E: $pass_count 項目 PASS"
echo "証拠: $WORK (logs + shots)"
