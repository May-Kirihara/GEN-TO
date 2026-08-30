#!/usr/bin/env python3
"""FG-1 最終ゲート修正の再計測ドライバ (release build + GENTO_DEV_BRIDGE=1)。

F7: s256全件生成 (空キャッシュ) ×3  → logs/s256-r{1..3}.log   (S2バッチ相当の保全)
    preview2048全件生成 (空キャッシュ) ×3 → logs/preview-r{1..3}.log (§4再計測)
F8: 5万件仮想カタログでPage_Down 40連打 ×3 → logs/v50k-scroll-r{1..3}.log
    (QA Minor-3埋め合わせ。§2 S3相当の仮想グリッド版)

値は devtest: ログ行 (1秒周期) の累積カウンタ差分とRustログから採取する。
結果は docs/dev/measurements/results.json へ追記 (fg1_* キー)。
"""
import json
import os
import re
import shutil
import signal
import subprocess
import time

BIN = "/mnt/holyland/Project/GEN-TO/target/release/gento-tauri"
DATASET = "/mnt/holyland/Project/GEN-TO/sample/dataset"
MEAS = "/mnt/holyland/Project/GEN-TO/docs/dev/measurements"
LOGS = f"{MEAS}/logs"
RESULTS = f"{MEAS}/results.json"
TMP = "/tmp/opencode/fg1"
ENV = dict(os.environ, DISPLAY=":1", GENTO_LOG="info", GENTO_DEV_BRIDGE="1", NO_COLOR="1")

ANSI = re.compile(r"\x1b\[[0-9;]*m")
DEVTEST = re.compile(r"devtest:\s*(.*)")


def launch(cache_dir, log_path):
    log = open(log_path, "ab", buffering=0)
    proc = subprocess.Popen(
        [BIN, "--dataset", DATASET, "--cache-dir", cache_dir],
        stdout=log, stderr=subprocess.STDOUT, env=ENV, cwd=TMP,
    )
    wids = []
    for _ in range(100):
        r = subprocess.run(["xdotool", "search", "--onlyvisible", "--pid", str(proc.pid)],
                           capture_output=True, text=True)
        if r.stdout.strip():
            wids = r.stdout.strip().split("\n")
            break
        time.sleep(0.2)
    time.sleep(1.5)
    if wids:
        subprocess.run(["xdotool", "windowactivate", "--sync", wids[0]], capture_output=True)
    return proc


def key(*args):
    subprocess.run(["xdotool", "key", *args], capture_output=True)


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
            try:
                snap[k] = float(v)
            except ValueError:
                snap[k] = v
        return snap
    return {}


def wait_for(log_path, pred, timeout=300, poll=0.25):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if pred(last_snap(read_log(log_path))):
            return True
        time.sleep(poll)
    return False


def num(snap, k, default=0.0):
    v = snap.get(k, default)
    return v if isinstance(v, (int, float)) else default


def record(scenario, run, data):
    results = {}
    if os.path.exists(RESULTS):
        with open(RESULTS) as f:
            results = json.load(f)
    results.setdefault(scenario, []).append({"run": run, **data})
    with open(RESULTS, "w") as f:
        json.dump(results, f, ensure_ascii=False, indent=1)
    print(f"[{scenario} run{run}] {data}", flush=True)


def terminate(proc, label, timeout=30):
    if proc.poll() is not None:
        return True
    proc.send_signal(signal.SIGTERM)
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            return True
        time.sleep(0.1)
    proc.kill()
    proc.wait()
    return False


def wait_generation_done(log_path, total=1615, timeout=900):
    """全件生成の完了待ち: q>0 が出て q==0 に戻り gen+fail が total 増える。"""
    base = last_snap(read_log(log_path))
    gen0, fail0 = num(base, "gen"), num(base, "fail")
    started = False
    t0 = time.monotonic()
    while time.monotonic() - t0 < timeout:
        s = last_snap(read_log(log_path))
        if num(s, "q") > 0:
            started = True
        if started and num(s, "q") == 0 and num(s, "gen") - gen0 + num(s, "fail") - fail0 >= total:
            return True, time.monotonic() - t0, s
        time.sleep(0.2)
    return False, time.monotonic() - t0, last_snap(read_log(log_path))


def fg1_s256(run):
    """F7: s256全件生成 (空キャッシュ)。S2相当を保全済みログとして再実施。"""
    cache = f"{TMP}/cache-s256-r{run}"
    shutil.rmtree(cache, ignore_errors=True)
    log_path = f"{LOGS}/s256-r{run}.log"
    open(log_path, "wb").close()
    proc = launch(cache, log_path)
    key("F8")
    ok = wait_for(log_path, lambda s: num(s, "tot") == 1615, 30)
    time.sleep(1.0)
    t0 = time.monotonic()
    max_q = 0
    key("ctrl+F8")
    done = False
    while time.monotonic() - t0 < 600:
        s = last_snap(read_log(log_path))
        max_q = max(max_q, num(s, "q"))
        if num(s, "q") == 0 and num(s, "gen") + num(s, "fail") >= 1615:
            done = True
            break
        time.sleep(0.2)
    elapsed = time.monotonic() - t0
    s = last_snap(read_log(log_path))
    record("fg1_s256_all", run, {
        "scan_ok": ok, "done": done, "elapsed_s": round(elapsed, 2),
        "max_queue": max_q,
        "p50_ms": num(s, "p50"), "p95_ms": num(s, "p95"), "p99_ms": num(s, "p99"),
        "fail": num(s, "fail"), "cache_bytes": num(s, "cb"),
    })
    return cache, proc, log_path


def fg1_preview(run):
    """F7: preview2048全件生成 (空キャッシュ)。§4 WebP評価の再計測。"""
    cache = f"{TMP}/cache-preview-r{run}"
    shutil.rmtree(cache, ignore_errors=True)
    log_path = f"{LOGS}/preview-r{run}.log"
    open(log_path, "wb").close()
    proc = launch(cache, log_path)
    key("F8")
    ok = wait_for(log_path, lambda s: num(s, "tot") == 1615, 30)
    time.sleep(1.0)
    t0 = time.monotonic()
    key("shift+F8")  # preview2048全件生成
    done, elapsed, s = wait_generation_done(log_path)
    record("fg1_preview_all", run, {
        "scan_ok": ok, "done": done, "elapsed_s": round(elapsed, 2),
        "generated": num(s, "gen"),
        "p50_ms": num(s, "p50"), "p95_ms": num(s, "p95"), "p99_ms": num(s, "p99"),
        "fail": num(s, "fail"), "cache_bytes": num(s, "cb"),
    })
    return proc


def fg1_v50k_scroll(run, cache):
    """F8: 5万件仮想カタログでPage_Down 40連打 (S3相当。ウォームキャッシュ)。"""
    log_path = f"{LOGS}/v50k-scroll-r{run}.log"
    open(log_path, "wb").close()
    proc = launch(cache, log_path)
    key("F8")
    wait_for(log_path, lambda s: num(s, "tot") == 1615, 30)
    key("F9")  # 仮想カタログへ
    ok_mode = wait_for(log_path, lambda s: num(s, "tot") == 50000, 30)
    key("ctrl+F2")  # グリッドへフォーカス
    time.sleep(1.0)
    key("Home")
    time.sleep(1.2)
    # ウォームアップ: 先頭可視範囲の要求がヒットしてreadyになるのを待つ
    wait_for(log_path, lambda s: num(s, "hr") == 1.0 and num(s, "hit") > 0, 60)
    before = last_snap(read_log(log_path))
    t0 = time.monotonic()
    key("--repeat", "40", "--delay", "40", "Page_Down")
    keys_wall = time.monotonic() - t0
    time.sleep(3.0)  # 収束待ち
    after = last_snap(read_log(log_path))
    time.sleep(1.5)
    settled = last_snap(read_log(log_path))
    record("fg1_v50k_scroll", run, {
        "mode_ok": ok_mode, "total": 50000,
        "pages_down": 40, "keys_wall_s": round(keys_wall, 2),
        "long_tasks": num(after, "lt") - num(before, "lt"),
        "lt_obs": num(before, "ltObs", -1),
        "long_frames": num(after, "lf") - num(before, "lf"),
        "worst_frame_ms": num(after, "lfMax"),
        "cancelled": num(after, "can") - num(before, "can"),
        "cancel_batches": num(after, "canB") - num(before, "canB"),
        "requests": num(after, "req") - num(before, "req"),
        "ready_recv": num(after, "rdy") - num(before, "rdy"),
        "generated": num(after, "gen") - num(before, "gen"),
        "q_after": num(after, "q"), "dom": num(after, "dom"),
        "q_settled": num(settled, "q"),
        "page_errors": num(after, "pe") - num(before, "pe"),
    })
    return proc


def main():
    os.makedirs(TMP, exist_ok=True)
    # F7: s256 ×3 (r3のキャッシュをF8のウォームキャッシュとして流用)
    for run in (1, 2, 3):
        cache, proc, _ = fg1_s256(run)
        terminate(proc, f"s256-r{run}")
        if run == 3:
            warm_cache = cache
    # F7: preview ×3
    for run in (1, 2, 3):
        proc = fg1_preview(run)
        terminate(proc, f"preview-r{run}")
    # F8: 仮想5万スクロール ×3 (ウォームキャッシュ)
    for run in (1, 2, 3):
        proc = fg1_v50k_scroll(run, warm_cache)
        terminate(proc, f"v50k-r{run}")
    print("FG1 REMEASURE DONE")


if __name__ == "__main__":
    main()
