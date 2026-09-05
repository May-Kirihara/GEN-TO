#!/usr/bin/env python3
"""WP-16 S4追試: 100MP画像のビューアoriginal表示 (WebKitデコード) の応答性。
fixturesからphoto100mp.pngのみのディレクトリを用意し、cell0=100MPでビューアを
開く。long frame・RSS・ハートビートを記録する。
"""
import json
import os
import re
import shutil
import subprocess
import threading
import time

BIN = "/mnt/holyland/Project/GEN-TO/target/release/gento-tauri"
WP16 = "/tmp/opencode/wp16"
WORK = f"{WP16}/s4view"
DS = f"{WORK}/ds"
ENV = dict(os.environ, DISPLAY=os.environ.get("GENTO_E2E_DISPLAY", ":6"),
           GENTO_LOG="info", GENTO_DEV_BRIDGE="1", NO_COLOR="1")
ANSI = re.compile(r"\x1b\[[0-9;]*m")
DEVTEST = re.compile(r"devtest:\s*(.*)")

shutil.rmtree(WORK, ignore_errors=True)
os.makedirs(f"{WORK}/home", exist_ok=True)
os.makedirs(DS, exist_ok=True)
os.makedirs(f"{WORK}/logs", exist_ok=True)
shutil.copy("/tmp/opencode/wp16/fixtures/photo100mp.png", DS)
shutil.copy("/tmp/opencode/wp16/fixtures/photo100mp.jpg", DS)

log_path = f"{WORK}/logs/app.log"
open(log_path, "wb").close()
env = dict(ENV, HOME=f"{WORK}/home", XDG_DATA_HOME=f"{WORK}/home/.local/share",
           XDG_CACHE_HOME=f"{WORK}/home/.cache")
log = open(log_path, "ab", buffering=0)
proc = subprocess.Popen(
    [BIN, "--dataset", DS, "--cache-dir", f"{WORK}/thumbs",
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


def key(*args):
    subprocess.run(["xdotool", "key", *args], capture_output=True, env=ENV)


def read_log():
    with open(log_path, "rb") as f:
        return f.read().decode("utf-8", errors="replace")


def last_snap():
    for line in reversed(read_log().splitlines()):
        clean = ANSI.sub("", line)
        m = DEVTEST.search(clean)
        if not m:
            continue
        snap = {}
        for token in re.finditer(r"([A-Za-z0-9_]+)=([^\s|]+)", m.group(1)):
            k, v = token.group(1), token.group(2)
            if k in snap:
                continue
            try:
                snap[k] = float(v)
            except ValueError:
                snap[k] = v
        return snap
    return {}


def num(snap, k, default=0.0):
    v = snap.get(k, default)
    return v if isinstance(v, (int, float)) else default


def rss_total(pid):
    total = 0

    def collect(p):
        nonlocal total
        try:
            with open(f"/proc/{p}/status") as f:
                for line in f:
                    if line.startswith("VmRSS:"):
                        total += int(line.split()[1])
                        break
        except OSError:
            return
        try:
            kids = open(f"/proc/{p}/task/{p}/children").read().split()
        except OSError:
            return
        for c in kids:
            collect(int(c))

    collect(pid)
    return total


results = {"rss_samples": []}
stop = threading.Event()


def sampler():
    while not stop.is_set():
        results["rss_samples"].append(rss_total(proc.pid))
        time.sleep(0.3)


threading.Thread(target=sampler, daemon=True).start()

# 登録 → 2asset確定待ち (ハッシュは速い)
key("ctrl+shift+F12"); time.sleep(0.4); key("0")
deadline = time.monotonic() + 60
while time.monotonic() < deadline:
    if num(last_snap(), "tot") >= 2:
        break
    time.sleep(0.5)
time.sleep(2)

# cell0 (photo100mp.jpg=名前順先頭) をビューアで開く
key("ctrl+F2"); time.sleep(0.5)
key("Home"); time.sleep(0.8)
before = last_snap()
rss0 = rss_total(proc.pid)
t0 = time.monotonic()
key("F2"); time.sleep(0.4)
key("F3")
# original昇格 (比較スロットでなく単一ビューアの原寸)。ズームインで原寸化を促す
# (UIUX-3: 単一ビューアの平常wheelは画像送りに変わったため、ズームは
#  vwCmdMode=素F12 → i のAlt+wheel橋アクションを使う。旧ctrl+F4は
#  metaRate5へroutingされておりwheelではなかった — F4bug修正済み)
time.sleep(5)
zoom_start = time.monotonic()
for _ in range(6):
    key("F12"); time.sleep(0.4); key("i")  # viewerAltZoomIn (dev bridge)
    time.sleep(0.5)
time.sleep(4)
after = last_snap()
results["viewer_100mp"] = {
    "open_to_settled_note_s": round(time.monotonic() - t0, 2),
    "long_frames": num(after, "lf") - num(before, "lf"),
    "worst_frame_ms": num(after, "lfMax"),
    "img_ok": num(after, "ok"), "img_err": num(after, "bad"),
    "rss_before_kb": rss0, "rss_after_kb": rss_total(proc.pid),
}
time.sleep(1)
results["rss_peak_kb"] = max(results["rss_samples"]) if results["rss_samples"] else 0
results["rss_final_kb"] = rss_total(proc.pid)

# ハートビート (5s超欠落なし=停止なし)
import datetime
ts = []
for line in read_log().splitlines():
    m = re.match(r"(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+)Z", line)
    if m and "devtest:" in line:
        ts.append(datetime.datetime.fromisoformat(m.group(1)))
results["heartbeat_gaps_gt5s"] = [
    (a.isoformat(), round((b - a).total_seconds(), 1))
    for a, b in zip(ts, ts[1:]) if (b - a).total_seconds() > 5.0]

stop.set()
proc.terminate()
proc.wait(timeout=30)
with open(f"{WORK}/results.json", "w") as f:
    json.dump(results, f, ensure_ascii=False, indent=1)
print(json.dumps(results, ensure_ascii=False, indent=1))
