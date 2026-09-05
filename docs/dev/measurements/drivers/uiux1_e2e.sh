#!/usr/bin/env bash
# UIUX-1 実機E2E検証 (devtoolsビルド + xdotool + Xディスプレイ)。
# 実行: bash docs/dev/measurements/drivers/uiux1_e2e.sh
#  (事前に pnpm build (apps/desktop) および
#   cargo build --release -p gento-tauri --features custom-protocol,devtools)
#
# 検証項目 (UIUX-1 / R-F② + r1レビューの追加テスト。uxCmdMode = shift+F1):
#   1) メニューバー: ファイル/表示/ツール/ヘルプの開閉 (UX xmen/xmi)・
#      クリック外閉じ
#   2) メニュー実操作: ツール>設定… (dialog) / ヘルプ>About直開 (xab) /
#      ツール>ルートのバックアップ… (登録後 BackupDialog) /
#      ファイル>SD学習用エクスポート…の無効条件 (xed=1・不発火)
#   3) タブドラフト生存: 手動コレクション作成フォーム (xcr) /
#      スマート条件フォーム (xcm=s) / ルート追加path入力 (xra) /
#      グリッドのリネーム入力 (xrn) — タブ切替を跨いで保存
#   4) パンくずパス入力: 絶対パス移動 (mode=real)・存在しないパスは
#      エラー通知 (xerr=1) でクラッシュしない
#   5) ファイル>一時走査を実行 (scan metricsログで実行証明)
#      + 一時走査を中止 (scn=1を観測してからメニューから中止 — r1)
#   6) FolderTree右クリック3状態: unrelated (登録導線) / root (3項目) /
#      descendant (メニュー無し) — r1
#   6c) / (ファイルシステム) 行登録の確認dialog: 表示 + Escape取消で
#      roots不変 (UIUX-2 — 実登録はしない) / 6d) タブラベル省略なし (C2)
#   7) メニューkeyboard操作: 矢印で開く・項目focus・ESCでトリガ復帰・
#      トップレベル矢印切替 (r1 — 実キー)
#   8) タブfocusモデル: 非アクティブパネルはhidden + focus不可 (xhf=0)
#   9) 表示メニュー: 左ペイン・右ペインの切替 (xlp/xrp)
#  10) エクスポート正ケース: コレクション優先 (xes=c) ・選択資産 (xes=s)
#  11) ツール実操作: AIパネル (xai) / 開発パネル (xdd) /
#      全DBバックアップ (xln=1) / 復元dialog (xdlg)
#  12) 実Quit: ファイル>終了 → window closeでプロセス終了 (SIGTERMと区別)
#  13) UIUX-3 V-1: スクロール速度 既定2.0・設定dialog表示タブでの編集/
#      永続化・「既定に戻す」・dialog閉時xsp=-1 (toolbar撤去)
#  14) UIUX-3 V-3: 検索行折りたたみ (要約=確定済み検索語+scope実態badge+
#      実行中表示中の✕クリア)・展開focus・再起動後も状態永続化
#  15) UIUX-3 V-4: ビューア送りボタン (端disabled)・平常wheel=送り
#      (accumulator: 3ノッチ=1送り)・Alt+wheelズーム・pinch (ctrl)ズーム
#  16) UIUX-3 V-2: 開発パネルのグリッド統計セクション (開中のみpoll)・
#      主UIオーバーレイ (.grid-stats) 撤去
#
# 運用ノート: uxlib1と同じ方針 (専用Xディスプレイ :6 = Xephyr。無ければ起動)。
# vw = 素F12のビューアwheel/navモード / u3 = 素F1のUIUX-3 UIモード
# (shift+字母キーはXephyr/WebKitへ到達しないため小文字専用モードを分離)。
set -uo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
BIN=/mnt/holyland/Project/GEN-TO/target/release/gento-tauri
WORK=/tmp/opencode/uiux1/e2e
# Xephyrのホスト表示は起動時のDISPLAY (上書き前に退避)
HOST_DISPLAY="${DISPLAY:-:0}"
DISPLAY="${GENTO_E2E_DISPLAY:-:6}"
export NO_COLOR=1 GENTO_LOG=info GENTO_DEV_BRIDGE=1

# キャンセルテスト用: 登録ルート配下に作るサブフォルダのファイル数
# (走査が十分長く続くよう多めに。scn観測は1秒粒度のreportCompact経由)
BIG=600

if ! xdpyinfo -display "$DISPLAY" >/dev/null 2>&1; then
  env DISPLAY="$HOST_DISPLAY" setsid nohup Xephyr "$DISPLAY" -screen 1600x1000 >/tmp/opencode/uiux1-xephyr.log 2>&1 </dev/null &
  for _ in $(seq 1 50); do
    xdpyinfo -display "$DISPLAY" >/dev/null 2>&1 && break
    sleep 0.2
  done
fi
xdpyinfo -display "$DISPLAY" >/dev/null 2>&1 || { echo "no X display on $DISPLAY" >&2; exit 1; }

rm -rf "$WORK"; mkdir -p "$WORK/home" "$WORK/env/thumbs" "$WORK/env/db" "$WORK/dataset" \
  "$WORK/home/AAA-unreg/sub" "$WORK/appcwd" "$WORK/shots" "$WORK/logs"
SHOT="$WORK/shots"
NOTE="$WORK/e2e-notes.txt"; : > "$NOTE"
shot() { import -window root "$SHOT/$1.png" 2>/dev/null || true; }
pass() { echo "PASS: $*" | tee -a "$NOTE"; PASSES=$((PASSES + 1)); }
fail() { echo "FAIL: $*" | tee -a "$NOTE"; FAILURES=$((FAILURES + 1)); }
PASSES=0
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

# fixture: dataset 4枚 + 登録外フォルダ 1枚 + サブ BIG枚 (実PNG。
# seedは重複しない範囲を使い、BLAKE3重複による資産統合を避ける)
python3 - "$WORK/dataset" "$WORK/home/AAA-unreg" "$WORK/home/AAA-unreg/sub" "$BIG" <<'PYEOF'
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
dataset, unreg, sub = (Path(a) for a in sys.argv[1:4])
big = int(sys.argv[4])
for i in range(4):
    png(dataset / f"ux-{i:02}.png", i)
png(unreg / "unreg.png", 9)
for i in range(big):
    png(sub / f"sub-{i:04}.png", 100 + i)
print(f"fixture: dataset 4 + AAA-unreg 1 + sub {big} pngs")
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

# コマンドモードhelpers: ux=shift+F1 (UIUX-1) / coll=ctrl+shift+F1 (WP-6) /
# fo=ctrl+shift+F12 (WP-7) / vw=素F12 (UIUX-3: ビューアwheel/nav) /
# u3=素F1 (UIUX-3: 設定速度/検索折りたたみ。shift+字母はXephyr/WebKitへ
# 到達しないため小文字専用の別モード)。
# いずれもモーメンタリ (1回のアクションで自動退出)
ux() { xdotool key shift+F1; sleep 0.4; xdotool key "$1"; sleep "${2:-1.2}"; }
coll() { xdotool key ctrl+shift+F1; sleep 0.4; xdotool key "$1"; sleep "${2:-1.2}"; }
fo() { xdotool key ctrl+shift+F12; sleep 0.4; xdotool key "$1"; sleep "${2:-1.2}"; }
vw() { xdotool key F12; sleep 0.4; xdotool key "$1"; sleep "${2:-1.0}"; }
u3() { xdotool key F1; sleep 0.4; xdotool key "$1"; sleep "${2:-1.2}"; }

# 検索S行のキー抽出 (セッション行も「S act=」を持つためq=入りの検索行形で
# 文脈grep — wp9_e2e.shのstotと同じ規約)
svalue() {
  local seg
  seg=$(sed 's/\x1b\[[0-9;]*m//g' "$WORK/logs/app.log" 2>/dev/null | grep -a "devtest:" | tail -1 \
    | grep -oE 'S act=[0-9] q="[^"]*"[^|]*')
  echo "$seg" | grep -oE "$1=[-0-9A-Za-z_.]*" | head -1 | cut -d= -f2
}

# ========== 起動 (未登録 dataset = adhoc) ==========
launch
wait_log "devtest:" 30 || { fail "0) dev bridge not flowing"; }
if wait_value mode real 20 && wait_value tot 4 20; then
  pass "0) 起動: adhoc表示・4件 (メニューバー+タブ付きレイアウトで描画)"
else
  fail "0) 起動不全 (mode=$(devtest_value mode || echo none) tot=$(devtest_value tot || echo none))"
fi
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

# ========== 7) メニューkeyboard操作 (実キー — r1) ==========
ux z 0.6    # uxFocusMenubar (ファイルボタンへfocus)
xdotool key Down; sleep 1.0   # トップレベルArrowDown = 開く+最初の有効項目へfocus
if [[ "$(devtest_value xmen || true)" == "f" && "$(devtest_value xmi || true)" == "3" \
  && "$(devtest_value xrole || true)" == "menuitem" ]]; then
  pass "7) 実キーArrowDownでメニュー開+項目focus (xmen=f xrole=menuitem)"
else
  fail "7) keyboard open不正 (xmen=$(devtest_value xmen || echo -) xrole=$(devtest_value xrole || echo -))"
fi
xdotool key Down; sleep 0.5; xdotool key Down; sleep 0.8  # 項目移動 (循環)
xdotool key Escape; sleep 1.0
if wait_value xmen - 10 && [[ "$(devtest_value xrole || true)" == "menuitem" ]] \
  && [[ "$(devtest_value ae || true)" == "BUTTON.menubar-button" ]]; then
  pass "7) 実キーEscapeで閉じてトリガへfocus復帰 (ae=BUTTON.menubar-button)"
else
  fail "7) keyboard close/復帰不正 (xmen=$(devtest_value xmen || echo -) ae=$(devtest_value ae || echo -))"
fi
xdotool key Right; sleep 0.8   # トップレベルArrowRight = focus移動 (メニューは開かない)
xdotool key Down; sleep 1.0    # 移動先 (表示) をArrowDownで開く
if [[ "$(devtest_value xmen || true)" == "v" ]]; then
  pass "7) トップレベルArrowRight→Downで表示メニューへ (xmen=v)"
else
  fail "7) ArrowRight切替不正 (xmen=$(devtest_value xmen || echo -))"
fi
ux 4 0.6

# ========== 8) 非アクティブペイン: hidden + focus不可 (r1) ==========
ux x 1.0    # uxTryFocusHiddenPane
if [[ "$(devtest_value xhf || true)" == "0" && "$(devtest_value xhid || true)" == "1" ]]; then
  pass "8) 非アクティブパネルはhidden (xhid=1) かつfocus不可 (xhf=0 — 実WebKit)"
else
  fail "8) hidden実効性不正 (xhid=$(devtest_value xhid || echo -) xhf=$(devtest_value xhf || echo -))"
fi

# ========== 9) 表示メニュー: ペイン切替 (r1) ==========
ux p 1.0    # 表示>左ペイン → off
if wait_value xlp 0 10 && [[ "$(devtest_value xtab || true)" == "-" ]]; then
  pass "9) 表示>左ペインで左ペイン非表示 (xlp=0 xtab=-)"
else
  fail "9) 左ペインoff不正 (xlp=$(devtest_value xlp || echo -) xtab=$(devtest_value xtab || echo -))"
fi
ux p 1.0    # 再度 → on
if wait_value xlp 1 10 && wait_value xtab f 10; then
  pass "9) 左ペイン復帰 (xlp=1 xtab=f — leftTab永続化)"
else
  fail "9) 左ペインon不正 (xlp=$(devtest_value xlp || echo -) xtab=$(devtest_value xtab || echo -))"
fi
ux j 1.0    # 表示>右ペイン → off
if ! wait_value xrp 0 10; then
  fail "9) 右ペインoff不正 (xrp=$(devtest_value xrp || echo -))"
else
  ux j 1.0  # → on
  if wait_value xrp 1 10; then
    pass "9) 表示>右ペインのoff/on (xrp 0→1)"
  else
    fail "9) 右ペインon復帰不正 (xrp=$(devtest_value xrp || echo -))"
  fi
fi

# ========== 3) タブドラフト生存: 手動コレクション ==========
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

# ========== 3b) タブドラフト生存: スマート条件フォーム (r1) ==========
ux y 1.0    # uxCollSmartForm (+ スマート → 名前input focus)
xdotool type --delay 20 "smart-draft"; sleep 0.5
if wait_value xcr 11 10; then
  pass "3) スマート条件フォームへ入力 (xcr=11)"
else
  fail "3) スマートフォーム入力不全 (xcr=$(devtest_value xcr || echo -))"
fi
ux 5 1.0; ux 6 1.0   # タブ往復
if [[ "$(devtest_value xcm || true)" == "s" && "$(devtest_value xcr || true)" == "11" ]]; then
  pass "3) スマート条件ドラフト生存 (xcm=s xcr=11)"
else
  fail "3) スマートドラフト不全 (xcm=$(devtest_value xcm || echo -) xcr=$(devtest_value xcr || echo -))"
fi
# フォームは残ってもstartCreateが名前を初期化するため以降へ影響しない

# ========== 3c) タブドラフト生存: ルート追加path入力 (r1) ==========
ux o 1.0    # uxFocusRootPathInput (ライブラリタブ + roots-add input)
rootpath="$WORK/home/AAA-unreg"
xdotool type --delay 20 "$rootpath"; sleep 0.5
rootlen=${#rootpath}
if wait_value xra "$rootlen" 10; then
  pass "3) ルート追加pathへ入力 (xra=$rootlen)"
else
  fail "3) ルートpath入力不全 (xra=$(devtest_value xra || echo -) 期待=$rootlen)"
fi
ux 5 1.0; ux 7 1.0   # タブ往復 (folders → library)
if [[ "$(devtest_value xra || true)" == "$rootlen" ]]; then
  pass "3) ルートpathドラフト生存 (xra=$rootlen)"
else
  fail "3) ルートpathドラフト不全 (xra=$(devtest_value xra || echo -) 期待=$rootlen)"
fi
# フォーカスを外す (入力は残す — 追加ボタンは押さない)

# ========== 4) パンくずパス入力: 移動成功 / エラー通知 ==========
ux 9 1.0    # uxFocusBreadcrumb
xdotool type --delay 25 "$WORK/home/AAA-unreg"; sleep 0.3
shot 06-breadcrumb-input
xdotool key Return; sleep 2.0
# 一時走査カタログは再帰的: AAA-unreg直下1 + sub $BIG
if wait_value mode real 15 && wait_value tot $((BIG + 1)) 15; then
  pass "4) パンくず入力で絶対パス移動 (mode=real tot=$((BIG + 1)): AAA-unreg再帰)"
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

# ========== 6) FolderTree右クリック → 登録 (unrelated状態) ==========
# ツリーのhomeを展開してAAA-unreg行を選択 (row-selectedをDOMへ出す)
xdotool key shift+F7; sleep 2.5
# 現在 adhoc で AAA-unreg 表示中 = ツリーの当該行が row-selected
ux 0 1.0    # treeContextCurrent (選択中行でcontext menu)
if wait_value menu 1 10; then
  pass "6) ツリー右クリックでcontext menu — unrelated (U menu=1)"
else
  fail "6) context menuが開かない (menu=$(devtest_value menu || echo -))"
fi
shot 07-tree-context-menu
ux a 2.0    # treeMenuRegister
if wait_value mode library 30; then
  pass "6) 「このフォルダーを登録ルートに追加」→ library表示へ自動切替 (mode=library)"
else
  fail "6) 登録導線不全 (mode=$(devtest_value mode || echo none))"
fi
if wait_value tot $((BIG + 1)) 40; then
  pass "6) 登録ルート表示 $((BIG + 1))件 (直下1+sub $BIG — 再帰登録)"
else
  fail "6) 登録ルート表示不全 (tot=$(devtest_value tot || echo -) 期待=$((BIG + 1)))"
fi
shot 08-registered-library

# ========== 6b) FolderTree右クリック: root状態 (3項目) と descendant (メニュー無し) — r1 ==========
# 登録済みパスはクリック表示がlibrary固定のため、バッジで行を特定して右クリック
ux h 1.0    # treeContextRoot → 登録ルート行でcontext menu
if wait_value menu 1 10 && wait_value mbtn 3 10; then
  pass "6) root右クリックは3項目 (解除/バックアップ/再スキャン — U mbtn=3)"
else
  fail "6) rootメニュー不正 (menu=$(devtest_value menu || echo -) mbtn=$(devtest_value mbtn || echo -))"
fi
xdotool key Escape; sleep 0.8    # メニューを閉じる
ux t 2.0    # treeExpandRoot (登録ルート行を展開)
if wait_value xti 1 15; then
  pass "6) 登録ルート配下のサブフォルダが「登録内」バッジで出現 (xti=1)"
else
  fail "6) 登録内バッジ不出現 (xti=$(devtest_value xti || echo -))"
fi
ux d 1.0    # treeContextInside (登録内行で右クリック)
if wait_value menu 0 10; then
  pass "6) descendant右クリックはメニュー無し (U menu=0)"
else
  fail "6) descendantでメニューが出た (menu=$(devtest_value menu || echo -))"
fi
shot 09-tree-descendant

# ========== 6c) 「ファイルシステム (/)」登録の確認dialog — UIUX-2 ==========
# 危険パス (depth≤1) の登録は確認dialogを挟む。Cancel = 登録されない
# (roots不変・表示遷移なし)。実際の登録は行わない (走査コスト・危険)。
roots_before=$(devtest_value xrt || echo -)
mode_before=$(devtest_value mode || echo -)
ux l 1.0    # treeContextFsRoot — / 行で右クリック (unrelated)
if wait_value menu 1 10; then
  pass "6) / 行右クリックでcontext menu (U menu=1)"
else
  fail "6) / 行のcontext menuが開かない (menu=$(devtest_value menu || echo -))"
fi
shot 14-fsroot-context-menu
ux a 1.0    # treeMenuRegister → 確認dialog
if wait_value xrc 1 10; then
  pass "6) / 登録で確認dialog表示 (xrc=1 — 無確認登録の解消)"
else
  fail "6) / 登録の確認dialogが出ない (xrc=$(devtest_value xrc || echo -))"
fi
shot 15-register-confirm
xdotool key Escape; sleep 1.0   # dialogをEscapeで取消
if wait_value xrc 0 10 \
  && [[ "$(devtest_value xrt || true)" == "$roots_before" ]] \
  && [[ "$(devtest_value mode || true)" == "$mode_before" ]]; then
  pass "6) 確認dialog取消で登録されない (xrc=0 roots/mode不変)"
else
  fail "6) 取消後の状態が不正 (xrc=$(devtest_value xrc || echo -) xrt=$(devtest_value xrt || echo -) 期待=$roots_before)"
fi

# ========== 6d) 左ペインタブのラベル省略なし (UIUX-2 C2) ==========
if [[ "$(devtest_value xtt || true)" == "0" ]]; then
  pass "6) 既定240pxでタブラベル省略なし (xtt=0)"
else
  fail "6) タブラベルが省略されている (xtt=$(devtest_value xtt || echo -))"
fi
shot 16-left-tabs

# ========== 2c) ツール>ルートのバックアップ… (登録済み → BackupDialog直開) ==========
ux k 1.5    # uxMenuBackupRoot
if wait_value xdlg 1 10; then
  pass "2) ツール>ルートのバックアップ… → BackupDialog開 (1ルート=直開, xdlg=1)"
else
  fail "2) BackupDialogが開かない (xdlg=$(devtest_value xdlg || echo -))"
fi
shot 10-backup-dialog
xdotool key Escape; sleep 0.8
wait_value xdlg 0 10 || fail "2) BackupDialogがEscapeで閉じない"

# ========== 10) エクスポート正ケース (コレクション優先) — r1 ==========
fo 1 1.5    # foShowLibraryRow → library表示
wait_value mode library 15 || fail "10) library表示へ戻れない (mode=$(devtest_value mode || echo -))"
coll 1 1.0   # collCreateManualForm
xdotool type --delay 25 "E2E Export"; sleep 0.3
xdotool key Return; sleep 1.5
if wait_value n 1 15; then
  pass "10) 手動コレクション「E2E Export」作成 (CL n=1)"
else
  fail "10) コレクション作成不全 (n=$(devtest_value n || echo -))"
fi
coll 4 0.8   # contextMenuCell0
coll 5 1.0   # 「コレクションへ追加…」→ ピッカー
coll 6 1.4   # 既存「E2E Export」へ追加
xdotool key Escape; sleep 0.6   # ピッカーを閉じる (残るとdialog数が増える)
wait_value xdlg 0 10 || true
coll 3 1.5   # collShowNext → E2E Exportを表示
if [[ "$(devtest_value kind || true)" == "m" ]] && wait_value cnt 1 15; then
  pass "10) コレクション表示 (CL kind=m cnt=1)"
else
  fail "10) コレクション表示不全 (kind=$(devtest_value kind || echo -) cnt=$(devtest_value cnt || echo -))"
fi
ux e 1.5    # uxMenuExport → コレクション優先
if wait_value xdlg 1 10 && wait_value xes c 10; then
  pass "10) エクスポート正: コレクション優先 (xdlg=1 xes=c)"
else
  fail "10) コレクション優先エクスポート不全 (xdlg=$(devtest_value xdlg || echo -) xes=$(devtest_value xes || echo -))"
fi
shot 11-export-collection
xdotool key Escape; sleep 0.8
wait_value xdlg 0 10 || fail "10) ExportDialogがEscapeで閉じない"

# ========== 10b) エクスポート正ケース (選択資産) — r1 ==========
fo 1 1.5    # library表示へ戻る
wait_value mode library 15 || fail "10) library表示へ戻れない (mode=$(devtest_value mode || echo -))"
xdotool key ctrl+shift+F4; sleep 1.0   # focusCell0 → 単一選択
if wait_value selN 1 10; then
  pass "10) 資産を単一選択 (G selN=1)"
else
  fail "10) 選択不全 (selN=$(devtest_value selN || echo -))"
fi
ux e 1.5    # uxMenuExport → 選択資産
if wait_value xdlg 1 10 && wait_value xes s 10; then
  pass "10) エクスポート正: 選択資産 (xdlg=1 xes=s)"
else
  fail "10) 選択資産エクスポート不全 (xdlg=$(devtest_value xdlg || echo -) xes=$(devtest_value xes || echo -))"
fi
shot 12-export-selection
xdotool key Escape; sleep 0.8
wait_value xdlg 0 10 || fail "10) ExportDialog (選択) がEscapeで閉じない"

# ========== 5b) 一時走査を中止 (scn観測→メニュー中止 — r1) ==========
# ローカルfolderの一時走査は一瞬で終わるため、長く続く対象として / を
# ツリーから開く (qa_wp4_e2e.shのcancel検証と同一方針 = ctrl+F7)。
# 中止はメニュー項目「一時走査を中止」(scanning中のみ存在 —
# runMenuItemは項目が無ければclickしない=安全)。
cancel_ok=0
xsc_seen="-"
xdotool key ctrl+F7; sleep 0.5   # openTreeFsRoot — / のadhoc走査が開始
for _attempt in 1 2 3; do
  scn_seen=0
  deadline=$((SECONDS + 10))
  while (( SECONDS < deadline )); do
    [[ "$(devtest_value scn || true)" == "1" ]] && { scn_seen=1; break; }
    sleep 0.2
  done
  (( scn_seen )) || { ux c 0.3; continue; }   # 走査再開を試す
  ux 1 0.8   # scanning中にファイルメニューを開き項目ラベルを観測
  xsc_seen=$(devtest_value xsc || true)
  ux n 1.5   # ファイル>一時走査を中止
  if wait_log "adhoc scan cancelled" 10; then cancel_ok=1; break; fi
  ux 4 0.4   # メニュー閉じ (走査終了待ちで再試行)
  sleep 1.0
done
if (( cancel_ok )) && ! grep -aq "scan metrics root=/ " "$WORK/logs/app.log"; then
  pass "5) ファイル>一時走査を中止 (scn観測→メニュー中止→backend到達・完了扱いなし xsc=$xsc_seen)"
else
  fail "5) 走査中止が検証できず (scn=$(devtest_value scn || echo -) xsc=$xsc_seen)"
fi

# ========== 3d) タブドラフト生存: リネーム入力 (r1) ==========
fo 1 1.5    # library表示 (リネームはasset対象のみ)
wait_value mode library 15 || fail "3) library表示へ戻れない (mode=$(devtest_value mode || echo -))"
xdotool key ctrl+shift+F4; sleep 0.8   # cell0単一選択
fo 5 0.8    # contextMenuFocused (選択セルのコンテキストメニュー)
fo e 1.2    # 「リネーム (F2)」→ インライン入力
xdotool key ctrl+a; sleep 0.3    # 既存値を選択 (置換で入力)
xdotool type --delay 25 "rename-draft"; sleep 0.5
if wait_value xrn 12 10; then
  pass "3) リネーム入力へドラフト (xrn=12)"
else
  fail "3) リネーム入力不全 (xrn=$(devtest_value xrn || echo -))"
fi
ux 5 1.0; ux 7 1.0   # 左タブ往復 (グリッドは中央ペイン — 編集が残るべき)
if [[ "$(devtest_value xrn || true)" == "12" ]]; then
  pass "3) リネームドラフト生存 (xrn=12 — タブ切替を跨いで維持)"
else
  fail "3) リネームドラフト不全 (xrn=$(devtest_value xrn || echo -))"
fi
xdotool key Escape; sleep 0.8   # リネーム取消 (xrn=-1へ)
wait_value xrn -1 10 || fail "3) リネームEscapeで取消れない (xrn=$(devtest_value xrn || echo -))"

# ========== 11) ツール実操作: AI / 開発 / 全DB / 復元 (r1) ==========
ux i 1.5    # ツール>AIパネル…
if wait_value xai 1 10; then
  xdotool key Escape; sleep 0.8
  if wait_value xai 0 10; then
    pass "11) ツール>AIパネル… 開閉 (xai 1→0)"
  else
    fail "11) AIパネルがEscapeで閉じない (xai=$(devtest_value xai || echo -))"
  fi
else
  fail "11) AIパネルが開かない (xai=$(devtest_value xai || echo -))"
fi
# dev bridge有効時は起動直後から開発パネルが開いている (App: devPanelOpen=enabled)。
# トグルの実効を両方向で確認: 1回目で閉じ (xdd 1→0)・2回目で再び開く (0→1)
ux g 1.2    # ツール>開発パネル (トグル)
if wait_value xdd 0 10; then
  ux g 1.2
  if wait_value xdd 1 10; then
    pass "11) ツール>開発パネル トグル (xdd 1→0→1)"
  else
    fail "11) 開発パネルが再び開かない (xdd=$(devtest_value xdd || echo -))"
  fi
else
  fail "11) 開発パネルが閉じない (xdd=$(devtest_value xdd || echo -))"
fi
ux f 1.5    # ツール>全データベースをバックアップ (即時実行)
if wait_value xln 1 20; then
  pass "11) ツール>全データベースをバックアップ (xln=1 — 完了通知)"
else
  fail "11) 全DBバックアップ完了通知なし (xln=$(devtest_value xln || echo -))"
fi
ux r 1.5    # ツール>バックアップから復元… (RestoreDialog)
if wait_value xdlg 1 10; then
  xdotool key Escape; sleep 0.8
  if wait_value xdlg 0 10; then
    pass "11) ツール>バックアップから復元… 開閉 (RestoreDialog xdlg 1→0)"
  else
    fail "11) RestoreDialogがEscapeで閉じない (xdlg=$(devtest_value xdlg || echo -))"
  fi
else
  fail "11) RestoreDialogが開かない (xdlg=$(devtest_value xdlg || echo -))"
fi
shot 13-tools-misc

# ========== 13) スクロール速度: 既定2.0・設定dialog表示タブで編集 (UIUX-3 V-1) ==========
# toolbarスライダーは撤去済み — xsp=は「u3 s (ツール>設定…→表示タブ直開)」後に
# 読む契約 (dialog閉時-1が正常値)。
u3 s 1.8
if [[ "$(devtest_value xdlg || true)" == "1" ]] && wait_value xsp 2 10; then
  pass "13) 設定表示タブで初期速度2.0 (xsp=2 — 既定2.0化・dialog移設)"
else
  fail "13) 初期速度/移設不正 (xdlg=$(devtest_value xdlg || echo -) xsp=$(devtest_value xsp || echo -))"
fi
shot 17-settings-display-speed
ux v 0.6    # dialog内のスライダーへfocus
xdotool key Right; sleep 0.25; xdotool key Right; sleep 0.25; xdotool key Right; sleep 0.9
if wait_value xsp 2.3 10; then
  pass "13) スライダー矢印編集 2.0→2.3 (input即時反映)"
else
  fail "13) スライダー編集不全 (xsp=$(devtest_value xsp || echo -) 期待=2.3)"
fi
xdotool key Escape; sleep 0.8
if wait_value xdlg 0 10 && wait_value xsp -1 10; then
  pass "13) dialog閉でスライダー不在 (xsp=-1 — toolbar撤去後の読み取り契約)"
else
  fail "13) dialog閉後もスライダーが見える (xdlg=$(devtest_value xdlg || echo -) xsp=$(devtest_value xsp || echo -))"
fi
u3 s 1.8
if wait_value xsp 2.3 10; then
  pass "13) 2.3が永続化 (change確定でlocalStorage保存・再オープンで復元)"
else
  fail "13) 速度永続化不全 (xsp=$(devtest_value xsp || echo -) 期待=2.3)"
fi
u3 r 1.2    # 「既定に戻す (2.0)」= 明示2.0書込
if wait_value xsp 2 10; then
  pass "13) 既定に戻す→2.0"
else
  fail "13) 既定に戻す不全 (xsp=$(devtest_value xsp || echo -))"
fi
xdotool key Escape; sleep 0.8
wait_value xdlg 0 10 || true

# ========== 14) 検索行折りたたみ (UIUX-3 V-3) ==========
# library表示 (605件) で検索「sub」を実行 → 確定済み検索状態を作ってから折りたたむ
fo h 0.8    # srFocusInput (展開中のみinputが存在)
xdotool type --delay 20 "sub"; sleep 0.3
xdotool key Return; sleep 1.8
sub_tot=$(svalue tot || echo -1)
if [[ "$(svalue act || true)" == "1" ]] && (( sub_tot > 0 )); then
  pass "14) 検索「sub」実行 (S act=1 tot=$sub_tot)"
else
  fail "14) 検索実行不全 (act=$(svalue act || echo -) tot=$sub_tot)"
fi
u3 e 1.4    # uxSearchToggle → 折りたたみ
if wait_value xsec 0 10; then
  pass "14) 検索行折りたたみ (xsec=0)"
else
  fail "14) 折りたたみ不全 (xsec=$(devtest_value xsec || echo -))"
fi
if [[ "$(devtest_value xsq || true)" == "3" && "$(devtest_value xssp || true)" == "l" ]] \
  && [[ "$(devtest_value xcl || true)" == "1" ]]; then
  pass "14) 折りたたみ要約: 確定済み検索語 (xsq=3='sub') + scope実態「ライブラリ全体」(xssp=l) + ✕クリア表示 (xcl=1)"
else
  fail "14) 折りたたみ要約不正 (xsq=$(devtest_value xsq || echo -) xssp=$(devtest_value xssp || echo -) xcl=$(devtest_value xcl || echo -))"
fi
shot 18-search-collapsed
u3 c 1.5    # ✕ 検索を解除 (入力クリア + 検索結果表示解除)
# 注: S行のact=はlastResult保持で1のまま — 解除はカタログがlibraryへ戻る
# ことで観測する (tot=605=登録ルート全件) + ✕ボタン消滅 (xcl=0)
if wait_value xcl 0 10 && wait_value tot $((BIG + 1)) 15; then
  pass "14) 折りたたみから✕クリア (検索結果表示を解除→library $((BIG + 1))件・✕消滅)"
else
  fail "14) ✕クリア不全 (tot=$(devtest_value tot || echo -) xcl=$(devtest_value xcl || echo -))"
fi
u3 e 1.4    # 再展開
if wait_value xsec 1 10 && [[ "$(devtest_value xrole || true)" == "input" ]]; then
  pass "14) 展開で検索inputへfocus (xsec=1 xrole=input)"
else
  fail "14) 展開focus不全 (xsec=$(devtest_value xsec || echo -) xrole=$(devtest_value xrole || echo -))"
fi
shot 19-search-expanded

# ========== 15) ビューア: 送りボタン・wheel送り・Alt/pinchズーム (UIUX-3 V-4) ==========
fo 1 1.5    # library表示へ戻す (検索解除済み)
wait_value mode library 15 || fail "15) library表示へ戻れない (mode=$(devtest_value mode || echo -))"
xdotool key ctrl+shift+F4; sleep 0.8   # focusCell0 → 単一選択
xdotool key F3; sleep 1.6              # openViewer (単一ビューア)
if [[ "$(devtest_value xvn || true)" == "2" && "$(devtest_value xvd || true)" == "1" ]]; then
  pass "15) 送りボタン表示・idx=0で前=disabled/次=有効 (xvn=2 xvd=1)"
else
  fail "15) 送りボタン初期状態不正 (xvn=$(devtest_value xvn || echo -) xvd=$(devtest_value xvd || echo -))"
fi
shot 20-viewer-nav
vw f 1.2    # 次ボタン
if wait_value idx 1 10 && wait_value xvd 0 10; then
  pass "15) 次ボタンでidx 0→1 (中間は両方有効)"
else
  fail "15) 次ボタン不全 (idx=$(devtest_value idx || echo -) xvd=$(devtest_value xvd || echo -))"
fi
vw b 1.2    # 前ボタン
if wait_value idx 0 10 && wait_value xvd 1 10; then
  pass "15) 前ボタンでidx 1→0 (先頭で前=disabled)"
else
  fail "15) 前ボタン不全 (idx=$(devtest_value idx || echo -) xvd=$(devtest_value xvd || echo -))"
fi
vw n 1.2    # 平常wheel (下) = 次へ。3ノッチdispatchでもaccumulatorで1枚だけ
if wait_value idx 1 10; then
  pass "15) 平常wheel=送り (idx 0→1・1ジェスチャ=1送り)"
else
  fail "15) wheel送り不全 (idx=$(devtest_value idx || echo -))"
fi
vw p 1.2    # 平常wheel (上) = 前へ
if wait_value idx 0 10; then
  pass "15) 平常wheel (上) =前の画像 (idx 1→0)"
else
  fail "15) wheel前送り不全 (idx=$(devtest_value idx || echo -))"
fi
zf0=$(devtest_value zf || echo -1)
vw i 1.2    # Alt+wheelズームイン
zf1=$(devtest_value zf || echo -1)
if [[ "$(devtest_value idx || true)" == "0" ]] && awk "BEGIN{exit !($zf1 > $zf0)}"; then
  pass "15) Alt+wheel=ズーム (zf $zf0→$zf1・idx不変)"
else
  fail "15) Altズーム不全 (idx=$(devtest_value idx || echo -) zf=$zf0→$zf1)"
fi
vw c 1.2    # ctrl+wheel (pinch) ズームイン
zf2=$(devtest_value zf || echo -1)
if [[ "$(devtest_value idx || true)" == "0" ]] && awk "BEGIN{exit !($zf2 > $zf1)}"; then
  pass "15) ctrl+wheel (pinch) もズームとして保存 (zf $zf1→$zf2)"
else
  fail "15) pinchズーム不全 (idx=$(devtest_value idx || echo -) zf=$zf1→$zf2)"
fi
xdotool key Escape; sleep 1.0
wait_value xvn -1 10 && pass "15) ビューア閉で送りボタン消滅 (xvn=-1)" \
  || fail "15) ビューア閉後もボタン残存 (xvn=$(devtest_value xvn || echo -))"

# ========== 16) 開発パネル: グリッド統計セクション (UIUX-3 V-2) ==========
# 開発パネルはセクション11で開状態 (xdd=1)。統計はパネル開中のみpoll表示。
xgs_v=$(devtest_value xgs || echo -1)
if (( xgs_v > 0 )); then
  pass "16) 開発パネルにグリッド統計表示 (xgs=$xgs_v — pull型poll)"
else
  fail "16) グリッド統計不出現 (xgs=$xgs_v)"
fi
if [[ "$(devtest_value xol || true)" == "0" ]]; then
  pass "16) 主UIに旧統計オーバーレイ無し (xol=0)"
else
  fail "16) オーバーレイ残存 (xol=$(devtest_value xol || echo -))"
fi
shot 21-devdrawer-grid-stats
# パネルを閉じると統計セクションも消える (poll停止)
ux g 1.4    # ツール>開発パネル (トグル→閉)
if wait_value xdd 0 10 && wait_value xgs -1 10; then
  pass "16) パネル閉で統計セクション消滅 (xdd=0 xgs=-1)"
else
  fail "16) パネル閉後の統計残存 (xdd=$(devtest_value xdd || echo -) xgs=$(devtest_value xgs || echo -))"
fi
ux g 1.4    # 再び開 (後続セクション・Quitのため)
wait_value xdd 1 10 || true

# ========== 14b) 検索折りたたみ状態の再起動永続化 (UIUX-3 V-3) ==========
u3 e 1.2    # 折りたたみへ
wait_value xsec 0 10 || fail "14) 永続化前の折りたたみ化に失敗 (xsec=$(devtest_value xsec || echo -))"
graceful_stop
launch
wait_log "devtest:" 30 || fail "14) 再起動後にdev bridgeが流れない"
if wait_value xsec 0 15; then
  pass "14) 折りたたみ状態が再起動後も復元 (xsec=0 — layout-prefs永続化)"
else
  fail "14) 折りたたみ永続化不全 (xsec=$(devtest_value xsec || echo -))"
fi
u3 e 1.2    # 展開へ戻す
wait_value xsec 1 15 || fail "14) 再展開不全 (xsec=$(devtest_value xsec || echo -))"

# ========== 12) 実Quit: ファイル>終了 → プロセス終了 (r1 — SIGTERMと区別) ==========
ux q 2.0    # uxMenuQuit → getCurrentWindow().close()
quit_ok=0
deadline=$((SECONDS + 20))
while (( SECONDS < deadline )); do
  if ! kill -0 "$APP_PID" 2>/dev/null; then quit_ok=1; break; fi
  sleep 0.3
done
if (( quit_ok )); then
  if grep -aq "panicked\|Panic" "$WORK/logs/app.log"; then
    fail "12) Quitでプロセス終了したがpanicが記録されている"
  else
    wait "$APP_PID" 2>/dev/null || true
    APP_PID=""
    pass "12) ファイル>終了でプロセス終了 (実window close — panic無し・SIGTERM不使用)"
  fi
else
  fail "12) ファイル>終了でプロセスが終了しない"
fi

# 条件数はpass/failの実呼び出しから動的に集計する (UIUX-3 N1:
# 手書き総数は実体とずれる — 46→52修正時に実際のpass呼び出しは50で
# 「50/52」と表示されていた。passのみ報告する条件 (|| fail のみの暗黙検査)
# は失敗時にのみ分母へ現れる点に注意)。
echo | tee -a "$NOTE"
echo "UIUX1 E2E RESULT: $PASSES/$((PASSES + FAILURES)) conditions passed, FAILURES=$FAILURES" | tee -a "$NOTE"
if (( FAILURES > 0 )); then
  echo "FAILING LINES:" | tee -a "$NOTE"
  grep -a "devtest:" "$WORK/logs/app.log" | tail -5 | tee -a "$NOTE"
  exit 1
fi
exit 0
