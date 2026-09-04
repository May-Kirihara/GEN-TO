#!/usr/bin/env bash
# UIUX-1 実機E2E検証 (devtoolsビルド + xdotool + Xディスプレイ)。
# 実行: bash docs/dev/measurements/drivers/uiux1_e2e.sh
#  (事前に pnpm build (apps/desktop) および
#   cargo build --release -p gento-tauri --features custom-protocol,devtools)
#
# 検証項目 (UIUX-1 / R-F②。dev bridgeのuxCmdMode = shift+F1で駆動):
#   1) メニューバー: ファイル/表示/ツール/ヘルプの開閉 (UX xmen/xmi)・
#      クリック外閉じ
#   2) メニュー実操作: ツール>設定… (dialog) / ヘルプ>About直開 (xab) /
#      ツール>ルートのバックアップ… (登録後 BackupDialog) /
#      ファイル>SD学習用エクスポート…の無効条件 (xed=1・不発火)
#   3) タブドラフト生存: コレクション作成フォーム入力 → タブ切替 → 復帰
#      で入力値が残る (全マウント維持)
#   4) パンくずパス入力: 絶対パス移動 (mode=real)・存在しないパスは
#      エラー通知 (xerr=1) でクラッシュしない
#   5) ファイル>一時走査を実行 (scan metricsログで実行証明)
#   6) FolderTree右クリック: 未登録フォルダ →「このフォルダーを登録ルートに
#      追加」→ library表示へ自動切替 (UXLIB-1導線)
#
# 運用メモ: uxlib1と同じ方針 (専用Xディスプレイ :6 = Xephyr。無ければ起動)。
set -uo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
BIN=/mnt/holyland/Project/GEN-TO/target/release/gento-tauri
WORK=/tmp/opencode/uiux1/e2e
# Xephyrのホスト表示は起動時のDISPLAY (上書き前に退避)
HOST_DISPLAY="${DISPLAY:-:0}"
DISPLAY="${GENTO_E2E_DISPLAY:-:6}"
export NO_COLOR=1 GENTO_LOG=info GENTO_DEV_BRIDGE=1

if ! xdpyinfo -display "$DISPLAY" >/dev/null 2>&1; then
  env DISPLAY="$HOST_DISPLAY" setsid nohup Xephyr "$DISPLAY" -screen 1600x1000 >/tmp/opencode/uiux1-xephyr.log 2>&1 </dev/null &
  for _ in $(seq 1 50); do
    xdpyinfo -display "$DISPLAY" >/dev/null 2>&1 && break
    sleep 0.2
  done
fi
xdpyinfo -display "$DISPLAY" >/dev/null 2>&1 || { echo "no X display on $DISPLAY" >&2; exit 1; }

rm -rf "$WORK"; mkdir -p "$WORK/home" "$WORK/env/thumbs" "$WORK/env/db" "$WORK/dataset" \
  "$WORK/home/AAA-unreg" "$WORK/appcwd" "$WORK/shots" "$WORK/logs"
SHOT="$WORK/shots"
NOTE="$WORK/e2e-notes.txt"; : > "$NOTE"
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

# fixture: dataset 4枚 + 登録外フォルダ 1枚 (実PNG。uxlib1と同一生成方式)
python3 - "$WORK/dataset" "$WORK/home/AAA-unreg" <<'PYEOF'
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
dataset, unreg = (Path(a) for a in sys.argv[1:3])
for i in range(4):
    png(dataset / f"ux-{i:02}.png", i)
png(unreg / "unreg.png", 9)
print("fixture: dataset 4 pngs + AAA-unreg 1 png")
PYEOF

APP_PID=""
launch() {
  : > "$WORK/logs/app.log"
  HOME="$WORK/home" "$BIN" --dataset "$WORK/dataset" \
    --cache-dir "$WORK/env/thumbs" --db "$WORK/env/db/library.sqlite3" \
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

# uxCmdModeアクション: shift+F1 → <key> (1回のアクション実行で自動退出)
ux() { xdotool key shift+F1; sleep 0.4; xdotool key "$1"; sleep "${2:-1.2}"; }

# ========== 起動 (未登録 dataset = adhoc) ==========
launch
wait_log "devtest:" 30 || fail "dev bridge not flowing"
wait_value mode real 20 || fail "起動がadhocでない (mode=$(devtest_value mode || echo none))"
wait_value tot 4 20 || fail "adhocカタログ4件 (tot=$(devtest_value tot || echo none))"
pass "0) 起動: adhoc表示・4件 (メニューバー+タブ付きレイアウトで描画)"
shot 00-initial

# ========== 1) メニューバー開閉 ==========
ux 1 0.8    # uxMenuFile
if [[ "$(devtest_value xmen || true)" == "f" && "$(devtest_value xmi || true)" == "3" ]]; then
  pass "1) ファイルメニュー開 (xmen=f, 項目xmi=3: 走査/エクスポート/終了)"
else
  fail "1) ファイルメニューが開けない (xmen=$(devtest_value xmen || echo -) xmi=$(devtest_value xmi || echo -))"
fi
shot 01-menu-file-open
ux 4 0.8    # uxMenuClose
if wait_value xmen - 10; then
  pass "1) メニュー閉じ (xmen=-)"
else
  fail "1) メニューが閉じない (xmen=$(devtest_value xmen || echo -))"
fi
ux 2 0.8    # uxMenuTools (devBridgeActive=1 → 開発パネル込み6項目)
if [[ "$(devtest_value xmen || true)" == "t" && "$(devtest_value xmi || true)" == "6" ]]; then
  pass "1) ツールメニュー (xmen=t, 項目xmi=6: AI/設定/バックアップ系3/開発パネル)"
else
  fail "1) ツールメニュー不正 (xmen=$(devtest_value xmen || echo -) xmi=$(devtest_value xmi || echo -))"
fi
ux 3 0.8    # uxMenuHelp
[[ "$(devtest_value xmen || true)" == "h" ]] && pass "1) ヘルプメニュー開 (xmen=h)" \
  || fail "1) ヘルプメニュー不正 (xmen=$(devtest_value xmen || echo -))"
ux 4 0.6

# ========== 2a) エクスポート無効条件 (選択・コレクションなし) ==========
ux e 1.0    # uxMenuExport: 無効項目click → 不発火・メニューは開いたまま
if wait_value xed 1 10 && [[ "$(devtest_value xdlg || true)" == "0" ]]; then
  pass "2) 非コレクション+選択0ではエクスポート無効 (xed=1)・dialog不発火 (xdlg=0)"
else
  fail "2) エクスポート無効条件が不正 (xed=$(devtest_value xed || echo -) xdlg=$(devtest_value xdlg || echo -))"
fi
ux 4 0.6

# ========== 2b) ツール>設定… / ヘルプ>About直開 ==========
ux s 1.2    # uxMenuSettings
if wait_value xdlg 1 10; then
  pass "2) ツール>設定… でdialog開 (xdlg=1)"
else
  fail "2) 設定dialogが開かない (xdlg=$(devtest_value xdlg || echo -))"
fi
xdotool key Escape; sleep 0.8
wait_value xdlg 0 10 || fail "2) 設定dialogがEscapeで閉じない"
ux b 1.2    # uxMenuAbout
if wait_value xab 1 10; then
  pass "2) ヘルプ>このアプリについて → Aboutタブ直開 (xab=1)"
else
  fail "2) About直開されない (xab=$(devtest_value xab || echo -))"
fi
shot 02-settings-about
xdotool key Escape; sleep 0.8
wait_value xdlg 0 10 || fail "2) About dialogがEscapeで閉じない"

# ========== 3) タブドラフト生存 ==========
ux 8 1.0    # uxCollCreateForm (コレクションタブ+作成フォーム+名前input focus)
xdotool type --delay 25 "draft-keep"; sleep 0.5
if wait_value xcr 10 10 && [[ "$(devtest_value xcm || true)" == "m" ]]; then
  pass "3) コレクション作成フォームへ入力 (xcr=10, manual)"
else
  fail "3) 作成フォーム入力不全 (xcr=$(devtest_value xcr || echo -) xcm=$(devtest_value xcm || echo -))"
fi
shot 03-tab-draft-before
ux 5 1.0    # uxTabFolders
[[ "$(devtest_value xtab || true)" == "f" ]] && pass "3) フォルダタブへ切替 (xtab=f)" \
  || fail "3) タブ切替不全 (xtab=$(devtest_value xtab || echo -))"
shot 04-tab-switched
ux 6 1.0    # uxTabCollections (ドラフト生存確認)
if [[ "$(devtest_value xtab || true)" == "c" && "$(devtest_value xcr || true)" == "10" ]]; then
  pass "3) タブ復帰でも作成ドラフト生存 (xcr=10 — 全マウント維持)"
else
  fail "3) ドラフトが失われた (xtab=$(devtest_value xtab || echo -) xcr=$(devtest_value xcr || echo -))"
fi
shot 05-tab-draft-after
# 作成フォームは開いたまま (以降のフェーズへ副作用なし・ドラフト生存状態を維持)

# ========== 4) パンくずパス入力: 移動成功 / エラー通知 ==========
ux 9 1.0    # uxFocusBreadcrumb
xdotool type --delay 25 "$WORK/home/AAA-unreg"; sleep 0.3
shot 06-breadcrumb-input
xdotool key Return; sleep 2.0
if wait_value mode real 15 && wait_value tot 1 15; then
  pass "4) パンくず入力で絶対パス移動 (mode=real tot=1: AAA-unreg)"
else
  fail "4) パス移動不全 (mode=$(devtest_value mode || echo none) tot=$(devtest_value tot || echo -))"
fi
if wait_value xbi 0 10; then
  pass "4) 移動後に入力欄が閉じる (xbi=0)"
else
  fail "4) 入力欄が残存 (xbi=$(devtest_value xbi || echo -))"
fi
# 存在しないパス → エラー通知・クラッシュしない
ux 9 1.0
xdotool type --delay 25 "/nonexistent-uiux1/xyz"; sleep 0.3
xdotool key Return; sleep 2.0
if [[ "$(devtest_value xerr || true)" == "1" ]] && wait_value mode real 10; then
  pass "4) 無効パスはエラー通知 (xerr=1)・表示維持 (クラッシュなし)"
else
  fail "4) 無効パスの扱い不正 (xerr=$(devtest_value xerr || echo -) mode=$(devtest_value mode || echo -))"
fi
wait_value xbi 0 10 || true

# ========== 5) ファイル>一時走査を実行 ==========
# 起動時の自動走査ログと区別するため、既存の "scan metrics" 行数を記録
scan_before=$(grep -ac "scan metrics" "$WORK/logs/app.log" || true)
ux c 1.5    # uxMenuScan
scan_ok=0
deadline=$((SECONDS + 30))
while (( SECONDS < deadline )); do
  scan_now=$(grep -ac "scan metrics" "$WORK/logs/app.log" || true)
  if (( scan_now > scan_before )); then scan_ok=1; break; fi
  sleep 0.3
done
if (( scan_ok )); then
  pass "5) ファイル>一時走査を実行 (scan metricsの新規出現=生産経路で走査実行)"
else
  fail "5) メニューからの走査が実行されない"
fi

# ========== 6) FolderTree右クリック → 登録 ==========
# ツリーのhomeを展開してAAA-unreg行を選択 (row-selectedをDOMへ出す)
xdotool key shift+F7; sleep 2.5
# 現在 adhoc で AAA-unreg 表示中 = ツリーの当該行が row-selected
ux 0 1.0    # treeContextCurrent (選択中行でcontext menu)
if wait_value menu 1 10; then
  pass "6) ツリー右クリックでcontext menu (U menu=1)"
else
  fail "6) context menuが開かない (menu=$(devtest_value menu || echo -))"
fi
shot 07-tree-context-menu
ux a 2.0    # treeMenuRegister
if wait_value mode library 20; then
  pass "6) 「このフォルダーを登録ルートに追加」→ library表示へ自動切替 (mode=library)"
else
  fail "6) 登録導線不全 (mode=$(devtest_value mode || echo none))"
fi
wait_value tot 1 20 || fail "6) 登録ルート表示1件 (tot=$(devtest_value tot || echo -))"
shot 08-registered-library

# ========== 2c) ツール>ルートのバックアップ… (登録済み → BackupDialog直開) ==========
ux k 1.5    # uxMenuBackupRoot
if wait_value xdlg 1 10; then
  pass "2) ツール>ルートのバックアップ… → BackupDialog開 (1ルート=直開, xdlg=1)"
else
  fail "2) BackupDialogが開かない (xdlg=$(devtest_value xdlg || echo -))"
fi
shot 09-backup-dialog
xdotool key Escape; sleep 0.8
wait_value xdlg 0 10 || fail "2) BackupDialogがEscapeで閉じない"

# ========== 終了 (graceful) ==========
if graceful_stop; then pass "7) SIGTERM graceful終了"; else fail "7) SIGTERMで終了しない"; fi

echo | tee -a "$NOTE"
echo "UIUX1 E2E RESULT: $((23 - FAILURES))/23 conditions passed, FAILURES=$FAILURES" | tee -a "$NOTE"
if (( FAILURES > 0 )); then
  echo "FAILING LINES:" | tee -a "$NOTE"
  grep -a "devtest:" "$WORK/logs/app.log" | tail -5 | tee -a "$NOTE"
  exit 1
fi
exit 0
