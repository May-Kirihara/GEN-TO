#!/usr/bin/env python3
"""WP-16 S9: ファイル操作中の異常終了 (kill -9) → 再起動整合性。

corpus1k (1000件) を登録したライブラリで複数選択→ゴミ箱/移動の実行中に
kill -9 → 再起動し、(a) .gento-tmp-* 残骸ゼロ (b) DB quick_check ok
(c) files表と実ディスクの一致 (d) カタログ表示  を検証する。

使い方: python3 docs/dev/measurements/drivers/wp16_s9_kill.py
"""
import json
import os
import re
import shutil
import signal
import sqlite3
import subprocess
import time

BIN = "/mnt/holyland/Project/GEN-TO/target/release/gento-tauri"
CORPUS1K = "/tmp/opencode/wp16/corpus1k"
WP16 = "/tmp/opencode/wp16"
WORK = f"{WP16}/s9"
ENV = dict(os.environ, DISPLAY=os.environ.get("GENTO_E2E_DISPLAY", ":6"),
           GENTO_LOG="info", GENTO_DEV_BRIDGE="1", NO_COLOR="1")

results = {}
ANSI = re.compile(r"\x1b\[[0-9;]*m")
DEVTEST = re.compile(r"devtest:\s*(.*)")


def snap(log_path):
    txt = open(log_path, errors="replace").read()
    for line in reversed(txt.splitlines()):
        if "devtest:" in line:
            d = {}
            for t in re.finditer(r"([A-Za-z0-9_]+)=([^\s|]+)", line.split("devtest:", 1)[1]):
                k = t.group(1)
                if k in d:
                    continue
                try:
                    d[k] = float(t.group(2))
                except ValueError:
                    d[k] = t.group(2)
            return d
    return {}


def wait_for(log_path, pred, timeout=60, poll=0.3):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if pred(snap(log_path)):
            return True
        time.sleep(poll)
    return False


def key(*args):
    subprocess.run(["xdotool", "key", *args], capture_output=True, env=ENV)


def launch(log_path):
    env = dict(ENV, HOME=f"{WORK}/home", XDG_DATA_HOME=f"{WORK}/home/.local/share",
               XDG_CACHE_HOME=f"{WORK}/home/.cache")
    log = open(log_path, "ab", buffering=0)
    proc = subprocess.Popen(
        [BIN, "--dataset", CORPUS1K, "--cache-dir", f"{WORK}/thumbs",
         "--db", f"{WORK}/home/library.sqlite3"],
        stdout=log, stderr=subprocess.STDOUT, env=env, cwd=WP16)
    r = ""
    for _ in range(150):
        r = subprocess.run(["xdotool", "search", "--onlyvisible", "--pid", str(proc.pid)],
                           capture_output=True, text=True, env=ENV).stdout.strip()
        if r:
            break
        time.sleep(0.2)
    time.sleep(1.5)
    if r:
        subprocess.run(["xdotool", "windowactivate", "--sync", r.split("\n")[0]],
                       capture_output=True, env=ENV)
    return proc


def db_q(sql):
    con = sqlite3.connect(f"file:{WORK}/home/library.sqlite3?mode=ro", uri=True, timeout=10)
    try:
        return con.execute(sql).fetchall()
    finally:
        con.close()


def count_files_on_disk():
    n = 0
    for _r, _d, fs in os.walk(CORPUS1K):
        n += len([f for f in fs if not f.startswith(".")])
    return n


def count_tmp():
    n = 0
    for root in (CORPUS1K, f"{WORK}/thumbs", f"{WORK}/home"):
        for _r, _d, fs in os.walk(root):
            n += sum(1 for f in fs if ".gento-tmp-" in f or f.startswith(".tmp-"))
    return n


def record(k, v):
    results[k] = v
    print(f"[{k}] {v}", flush=True)
    with open(f"{WORK}/results.json", "w") as f:
        json.dump(results, f, ensure_ascii=False, indent=1)


def main():
    shutil.rmtree(WORK, ignore_errors=True)
    os.makedirs(f"{WORK}/home", exist_ok=True)
    os.makedirs(f"{WORK}/logs", exist_ok=True)
    log_path = f"{WORK}/logs/phase1.log"
    open(log_path, "wb").close()

    # phase 1: 登録+走査+ハッシュ完了
    proc = launch(log_path)
    # マウント確認してから登録 (早すぎるキーが無視されるのを防ぐ)
    wait_for(log_path, lambda s: s.get("tot", 0) > 0, 60)
    time.sleep(1.5)
    for attempt in range(5):
        key("ctrl+shift+F12"); time.sleep(0.5); key("0")
        time.sleep(5)
        try:
            roots = db_q("SELECT COUNT(*) FROM library_roots")[0][0]
        except Exception:
            roots = 0
        if roots >= 1:
            break
    # ライブラリ行を表示 (foShowLibraryRow = "1")
    key("ctrl+shift+F12"); time.sleep(0.4); key("1")
    t0 = time.time()
    while time.time() - t0 < 600:
        try:
            assets = db_q("SELECT COUNT(*) FROM assets")[0][0]
            hashed = db_q("SELECT COUNT(*) FROM assets WHERE content_hash IS NOT NULL")[0][0]
            files = db_q("SELECT COUNT(*) FROM files")[0][0]
        except Exception:
            assets = hashed = files = 0
        if files >= 1000 and assets > 0 and hashed >= assets and assets >= 950:
            # 5s安定
            time.sleep(5)
            hashed2 = db_q("SELECT COUNT(*) FROM assets WHERE content_hash IS NOT NULL")[0][0]
            assets2 = db_q("SELECT COUNT(*) FROM assets")[0][0]
            if hashed2 >= assets2 and assets2 >= 950:
                break
        time.sleep(2)
    record("phase1", {"files": files, "assets": assets, "elapsed_s": round(time.time() - t0, 1)})

    # phase 2: 30件選択 → ゴミ箱実行 → 即kill -9
    key("ctrl+F2"); time.sleep(0.8)
    key("Home"); time.sleep(1.0)
    key("ctrl+shift+F4"); time.sleep(0.6)  # focusCell0
    for _ in range(29):
        key("Shift+Down"); time.sleep(0.06)
    time.sleep(0.8)
    s = snap(log_path)
    record("selection", {"selN": s.get("selN"), "foc": s.get("foc")})
    # ゴミ箱: 実Deleteキー (wp7_e2e.sh §6と同一の生産経路)
    key("Delete")
    killed_busy = False
    t_k = time.time()
    while time.time() - t_k < 5.0:
        s = snap(log_path)
        if s.get("busy") == 1 or (isinstance(s.get("prog"), str) and "/" in s.get("prog", "")):
            killed_busy = True
            break
        time.sleep(0.03)
    os.kill(proc.pid, signal.SIGKILL)
    os.waitpid(proc.pid, 0)
    time.sleep(1.0)
    record("killed", {
        "killed_while_busy": killed_busy,
        "reaction_ms": round((time.time() - t_k) * 1000, 0),
        "tmp_after_kill": count_tmp(),
        "disk_files_after_kill": count_files_on_disk(),
        "db_present_after_kill": db_q("SELECT COUNT(*) FROM files WHERE scan_state='present'")[0][0],
        "db_quickcheck": db_q("PRAGMA quick_check")[0][0],
    })

    # phase 3: 再起動 → reconciliation → 整合性
    log2 = f"{WORK}/logs/phase2.log"
    open(log2, "wb").close()
    t0 = time.time()
    proc = launch(log2)
    boot = time.time() - t0
    ok_tot = wait_for(log2, lambda s: s.get("tot", 0) >= 900, 120)
    time.sleep(12)  # 再走査・watcher反映・reconciliation待ち
    s = snap(log2)
    disk = count_files_on_disk()
    db_present = db_q("SELECT COUNT(*) FROM files WHERE scan_state='present'")[0][0]
    record("restart_reconciliation", {
        "boot_wall_s": round(boot, 2),
        "catalog_tot": s.get("tot"), "catalog_ok": ok_tot,
        "disk_files": disk, "db_present": db_present,
        "match": disk == db_present,
        "tmp_files": count_tmp(),
        "db_quickcheck": db_q("PRAGMA quick_check")[0][0],
        "lf": s.get("lf"), "lfMax": s.get("lfMax"),
        "hr": s.get("hr"),
    })
    proc.send_signal(signal.SIGTERM)
    try:
        proc.wait(timeout=30)
    except Exception:
        proc.kill()
    print("S9 DONE")


if __name__ == "__main__":
    main()
