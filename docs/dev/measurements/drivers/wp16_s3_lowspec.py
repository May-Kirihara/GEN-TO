#!/usr/bin/env python3
"""WP-16 S3: 低スペック相当 (taskset 2コア束縛) での主要操作計測。

taskset -c 0,1 でアプリ (本体+WebKit子プロセス) を2コアに束縛し、
起動・走査・s256全件生成・スクロール・検索を計測する。
GENTO_THUMB_WORKERS / GENTO_SCAN_BATCH ノブの効果検証も含む
(既定4ワーカー vs ノブで2ワーカー縮小の比較)。

使い方: python3 docs/dev/measurements/drivers/wp16_s3_lowspec.py
"""
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time

BIN = "/mnt/holyland/Project/GEN-TO/target/release/gento-tauri"
DATASET = "/mnt/holyland/Project/GEN-TO/sample/dataset"
WP16 = "/tmp/opencode/wp16"
WORK = f"{WP16}/s3"
ENV = dict(os.environ, DISPLAY=os.environ.get("GENTO_E2E_DISPLAY", ":6"),
           GENTO_LOG="info", GENTO_DEV_BRIDGE="1", NO_COLOR="1")

results = {}


def launch(cache_dir, log_path, db_path, home, extra_env=None):
    log = open(log_path, "ab", buffering=0)
    env = dict(ENV, HOME=home, XDG_DATA_HOME=f"{home}/.local/share",
               XDG_CACHE_HOME=f"{home}/.cache")
    if extra_env:
        env.update(extra_env)
    proc = subprocess.Popen(
        ["taskset", "-c", "0,1", BIN, "--dataset", DATASET,
         "--cache-dir", cache_dir, "--db", db_path],
        stdout=log, stderr=subprocess.STDOUT, env=env, cwd=WP16,
    )
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


def key(*args):
    subprocess.run(["xdotool", "key", *args], capture_output=True, env=ENV)


ANSI = re.compile(r"\x1b\[[0-9;]*m")
DEVTEST = re.compile(r"devtest:\s*(.*)")
SCAN_METRICS = re.compile(
    r"scan metrics.*found=(\d+).*scan_elapsed_ms=(\d+).*probe_elapsed_ms=(\d+).*rss_kb=(\d+)")


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


def terminate(proc):
    proc.send_signal(signal.SIGTERM)
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            return True
        time.sleep(0.1)
    proc.kill()
    return False


def run_case(name, thumb_workers=None, scan_batch=None):
    """1ケース: 起動→走査→s256全件→検索→スクロール (2コア束縛)。"""
    cache = f"{WORK}/cache-{name}"
    home = f"{WORK}/home-{name}"
    shutil.rmtree(cache, ignore_errors=True)
    shutil.rmtree(home, ignore_errors=True)
    os.makedirs(home, exist_ok=True)
    log_path = f"{WORK}/logs/{name}.log"
    os.makedirs(f"{WORK}/logs", exist_ok=True)
    open(log_path, "wb").close()
    extra = {}
    if thumb_workers:
        extra["GENTO_THUMB_WORKERS"] = str(thumb_workers)
    if scan_batch:
        extra["GENTO_SCAN_BATCH"] = str(scan_batch)

    t0 = time.monotonic()
    proc = launch(cache, log_path, f"{home}/library.sqlite3", home, extra)
    boot_wall = time.monotonic() - t0
    key("F8")
    ok_scan = wait_for(log_path, lambda s: num(s, "tot") == 1615, 60)
    sm = SCAN_METRICS.findall(ANSI.sub("", read_log(log_path)))
    scan_ms = int(sm[-1][1]) if sm else -1
    s = last_snap(read_log(log_path))
    record(f"{name}_startup", {
        "boot_wall_s": round(boot_wall, 2), "scan_ok": ok_scan,
        "scan_ms": scan_ms, "dH": num(s, "dH"), "dP": num(s, "dP"),
        "lfMax": num(s, "lfMax"),
        "workers": thumb_workers or "default", "scan_batch": scan_batch or "default",
    })

    # s256全件生成
    t0 = time.monotonic()
    key("ctrl+F8")
    max_q = 0
    done = False
    while time.monotonic() - t0 < 900:
        s = last_snap(read_log(log_path))
        max_q = max(max_q, num(s, "q"))
        if num(s, "q") == 0 and num(s, "gen") + num(s, "fail") >= 1615:
            done = True
            break
        time.sleep(0.5)
    elapsed = time.monotonic() - t0
    s = last_snap(read_log(log_path))
    record(f"{name}_gen_all", {
        "done": done, "elapsed_s": round(elapsed, 2), "max_queue": max_q,
        "p50_ms": num(s, "p50"), "p95_ms": num(s, "p95"), "p99_ms": num(s, "p99"),
        "fail": num(s, "fail"),
        "throughput_img_s": round(1615 / elapsed, 1) if done else None,
    })

    # 検索
    def search(q):
        key("ctrl+shift+F12"); time.sleep(0.4); key("h"); time.sleep(0.5)
        key("--delay", "40", "type", q); time.sleep(0.3)
        key("Return")
        ok = wait_for(log_path, lambda s: s.get("q") == q and num(s, "run") == 0, 60)
        time.sleep(0.8)
        s = last_snap(read_log(log_path))
        return {"query": q, "ok": ok, "tot": num(s, "tot"), "ms": num(s, "ms")}

    record(f"{name}_search", search("cat"))

    # スクロール (生成済みキャッシュで)
    key("Escape"); time.sleep(0.6)
    key("ctrl+F2"); time.sleep(0.8)
    key("Home"); time.sleep(1.2)
    before = last_snap(read_log(log_path))
    t0 = time.monotonic()
    key("--repeat", "40", "--delay", "40", "Page_Down")
    keys_wall = time.monotonic() - t0
    time.sleep(4.0)
    after = last_snap(read_log(log_path))
    record(f"{name}_scroll", {
        "keys_wall_s": round(keys_wall, 2),
        "long_frames": num(after, "lf") - num(before, "lf"),
        "worst_frame_ms": num(after, "lfMax"),
        "dom": num(after, "dom"), "q_settled": num(after, "q"),
    })
    record(f"{name}_rss", {"rss_kb": rss_total(proc.pid)})
    terminate(proc)
    # ノブ確認: 上書きログ行
    for line in read_log(log_path).splitlines():
        if "override active" in line:
            results.setdefault("knob_logs", []).append({"case": name, "line": line.strip()[-80:]})


def main():
    os.makedirs(f"{WORK}/logs", exist_ok=True)
    # 2コア + 既定ノブ (worker=4のまま)
    run_case("low2c_default")
    # 2コア + ノブ縮小 (workers=2・batch=16)
    run_case("low2c_knobs", thumb_workers=2, scan_batch=16)
    with open(f"{WORK}/results.json", "w") as f:
        json.dump(results, f, ensure_ascii=False, indent=1)
    print("S3 DONE")


if __name__ == "__main__":
    main()
