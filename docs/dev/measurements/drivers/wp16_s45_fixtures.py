#!/usr/bin/env python3
"""WP-16 S4/S5: 100MP級・破損・不正寸法・巨大メタデータfixtureのスポット計測。

前提: scripts/wp16_gen_fixtures.py で生成した /tmp/opencode/wp16/fixtures。
孤立したenvで起動 → データセット登録 → 走査+ハッシュ+サムネイルを待ち、
エラー分類 (アプリログ) とサムネイル統計 (T行)・RSSを記録する。
さらにビューアで100MP original表示 (WebKit側デコード) の応答性を見る。

使い方: python3 docs/dev/measurements/drivers/wp16_s45_fixtures.py
"""
import json
import os
import re
import shutil
import signal
import subprocess
import time

BIN = "/mnt/holyland/Project/GEN-TO/target/release/gento-tauri"
FIX = "/tmp/opencode/wp16/fixtures"
WP16 = "/tmp/opencode/wp16"
WORK = f"{WP16}/s45"
ENV = dict(os.environ, DISPLAY=os.environ.get("GENTO_E2E_DISPLAY", ":6"),
           GENTO_LOG="info", GENTO_DEV_BRIDGE="1", NO_COLOR="1")

results = {}
ANSI = re.compile(r"\x1b\[[0-9;]*m")
DEVTEST = re.compile(r"devtest:\s*(.*)")


def read_log(path):
    with open(path, "rb") as f:
        return f.read().decode("utf-8", errors="replace")


def last_snap(log_text):
    for line in reversed(log_text.splitlines()):
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


def wait_for(log_path, pred, timeout=60, poll=0.25):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if pred(last_snap(read_log(log_path))):
            return True
        time.sleep(poll)
    return False


def num(snap, k, default=0.0):
    v = snap.get(k, default)
    return v if isinstance(v, (int, float)) else default


def record(scenario, data):
    results.setdefault(scenario, []).append(data)
    print(f"[{scenario}] {data}", flush=True)
    with open(f"{WORK}/results.json", "w") as f:
        json.dump(results, f, ensure_ascii=False, indent=1)


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


def rss_peak_sampler(stop, pid_holder, out, interval=0.5):
    def run():
        peak = 0
        while not stop.is_set():
            if pid_holder.get("pid"):
                r = rss_total(pid_holder["pid"])
                peak = max(peak, r)
                out.append(r)
            time.sleep(interval)
        out.sort()
        pid_holder["peak"] = peak
    import threading
    t = threading.Thread(target=run, daemon=True)
    t.start()
    return t


def main():
    import threading
    shutil.rmtree(WORK, ignore_errors=True)
    os.makedirs(f"{WORK}/home", exist_ok=True)
    os.makedirs(f"{WORK}/logs", exist_ok=True)
    log_path = f"{WORK}/logs/app.log"
    open(log_path, "wb").close()

    env = dict(ENV, HOME=f"{WORK}/home", XDG_DATA_HOME=f"{WORK}/home/.local/share",
               XDG_CACHE_HOME=f"{WORK}/home/.cache")
    t0 = time.monotonic()
    log = open(log_path, "ab", buffering=0)
    proc = subprocess.Popen(
        [BIN, "--dataset", FIX, "--cache-dir", f"{WORK}/thumbs",
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

    # RSSピーク監視開始
    stop = threading.Event()
    holder = {"pid": proc.pid}
    samples = []
    rss_peak_sampler(stop, holder, samples)

    # 登録 + 走査 + ハッシュ + サムネ完了待ち (5ファイル対象)
    key("ctrl+shift+F12"); time.sleep(0.4); key("0")
    ok_tot = wait_for(log_path, lambda s: num(s, "tot") >= 3, 120)
    time.sleep(2)
    # ハッシュ+サムネ完了 (q=0 かつ T行が安定)
    time.sleep(6)
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline:
        s = last_snap(read_log(log_path))
        if num(s, "q") == 0 and (num(s, "gen") + num(s, "fail")) >= 1:
            break
        time.sleep(1.0)
    time.sleep(3)
    s = last_snap(read_log(log_path))
    record("fixture_stats", {
        "tot": num(s, "tot"), "tot_ok": ok_tot,
        "thumb_gen": num(s, "gen"), "thumb_fail": num(s, "fail"),
        "thumb_hit": num(s, "hit"), "thumb_miss": num(s, "mis"),
        "p50_ms": num(s, "p50"), "p95_ms": num(s, "p95"), "p99_ms": num(s, "p99"),
        "lf": num(s, "lf"), "lfMax": num(s, "lfMax"),
        "rss_kb": rss_total(proc.pid),
    })
    stop.set()
    time.sleep(1.2)
    results["rss_peak_kb"] = holder.get("peak", 0)
    results["rss_samples"] = samples[-10:]

    # 100MPをビューアで開く (original表示はWebKitがデコード)
    key("ctrl+F2"); time.sleep(0.6)
    key("Home"); time.sleep(0.8)
    before = last_snap(read_log(log_path))
    key("F2"); time.sleep(0.4)
    key("F3")
    time.sleep(6)
    after = last_snap(read_log(log_path))
    record("viewer_100mp", {
        "long_frames": num(after, "lf") - num(before, "lf"),
        "worst_frame_ms": num(after, "lfMax"),
        "img_ok": num(after, "ok"), "img_err": num(after, "bad"),
        "rss_kb": rss_total(proc.pid),
        "devtest_alive": len(read_log(log_path).splitlines()) > 0,
    })
    key("Escape"); time.sleep(0.8)

    # アプリログからエラー分類 (ハッシュ/プローブ/サムネの各エラー行)
    text = read_log(log_path)
    errors = {}
    for m in re.finditer(r"(decode failed[^\"]*|unreadable:[^\"]*|limits[^\"]*|"
                         r"decode error[^\"]*|probe[^\"]*)", text):
        e = ANSI.sub("", m.group(0))[:120]
        errors[e] = errors.get(e, 0) + 1
    results["error_classification"] = errors

    proc.send_signal(signal.SIGTERM)
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            break
        time.sleep(0.1)
    with open(f"{WORK}/results.json", "w") as f:
        json.dump(results, f, ensure_ascii=False, indent=1)
    print("S45 DONE")


if __name__ == "__main__":
    main()
