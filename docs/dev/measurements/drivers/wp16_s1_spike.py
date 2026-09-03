#!/usr/bin/env python3
"""WP-16 S1: スパイク計測の回帰チェック (release build + GENTO_DEV_BRIDGE=1)。

docs/technical-spike-report.md §4 (2026-08-30スパイク) と同一手順で
現行buildを計測し、退行比較の生数値を /tmp/opencode/wp16/s1-results.json へ
書き出す。スパイクの母体ドライバ run_b.py (WP-7) を簡素化して再利用。

対応: 起動(s1) / s256全件生成(s2) / キャッシュ済み再起動(s8) /
削除→再生成(s10) / 高速スクロール(s3) / 100枚移動(s4) /
5万件仮想ジャンプ(s9) / RSS idle (smaps_rollupは別コマンドで採取)

使い方 (Xephyr :6 を想定。corpus生成等と並走させないこと):
  python3 docs/dev/measurements/drivers/wp16_s1_spike.py
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
ENV = dict(os.environ, DISPLAY=os.environ.get("GENTO_E2E_DISPLAY", ":6"),
           GENTO_LOG="info", GENTO_DEV_BRIDGE="1", NO_COLOR="1")

results = {}


def launch(cache_dir, log_path, db_path=None, home=None):
    log = open(log_path, "ab", buffering=0)
    cmd = [BIN, "--dataset", DATASET, "--cache-dir", cache_dir]
    if db_path:
        cmd += ["--db", db_path]
    env = dict(ENV)
    if home:
        # HOME隔離: 既定DB (~/.local/share/gento-spike) を汚さない
        env["HOME"] = home
        env["XDG_DATA_HOME"] = f"{home}/.local/share"
        env["XDG_CACHE_HOME"] = f"{home}/.cache"
    proc = subprocess.Popen(
        cmd, stdout=log, stderr=subprocess.STDOUT, env=env, cwd=WP16,
    )
    r = ""
    for _ in range(100):
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


def rss_sample(pid, label):
    total = 0
    main = 0
    procs = 0

    def collect(p):
        nonlocal total, main, procs
        try:
            with open(f"/proc/{p}/status") as f:
                for line in f:
                    if line.startswith("VmRSS:"):
                        kb = int(line.split()[1])
                        total += kb
                        procs += 1
                        if p == pid:
                            main = kb
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
    return {"label": label, "rss_total_kb": total, "rss_main_kb": main, "procs": procs}


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
        # 同名キーが複数行に現れる (G tot= / S tot= / セッションtot=) ため
        # 「最初の出現」(グリッド行) を正とする (setdefault)
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


def scan_metrics(log_text):
    rows = SCAN_METRICS.findall(ANSI.sub("", log_text))
    return [{"found": int(r[0]), "scan_ms": int(r[1]), "probe_ms": int(r[2]),
             "rss_kb": int(r[3])} for r in rows]


def num(snap, k, default=0.0):
    v = snap.get(k, default)
    return v if isinstance(v, (int, float)) else default


def record(scenario, run, data):
    results.setdefault(scenario, []).append({"run": run, **data})
    print(f"[{scenario} run{run}] {data}", flush=True)
    with open(f"{WP16}/s1-results.json", "w") as f:
        json.dump(results, f, ensure_ascii=False, indent=1)


def terminate(proc):
    proc.send_signal(signal.SIGTERM)
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            return True
        time.sleep(0.1)
    proc.kill()
    return False


def session_A(run):
    """S1 (空キャッシュ起動) + S2 (s256全件生成)。"""
    cache = f"{WP16}/cache-main-r{run}"
    shutil.rmtree(cache, ignore_errors=True)
    home = f"{WP16}/home-r{run}"
    shutil.rmtree(home, ignore_errors=True)
    os.makedirs(home, exist_ok=True)
    log_path = f"{WP16}/logs/s1s2-r{run}.log"
    os.makedirs(f"{WP16}/logs", exist_ok=True)
    open(log_path, "wb").close()
    t0 = time.monotonic()
    proc = launch(cache, log_path, db_path=f"{home}/library.sqlite3", home=home)
    boot_wall = time.monotonic() - t0
    key("F8")
    ok = wait_for(log_path, lambda s: num(s, "tot") == 1615, 30)
    s1 = {"boot_wall_s": round(boot_wall, 2), "scan_ok": ok}
    for k in ("dH", "dP", "mnt", "lfMax"):
        s1[k] = num(last_snap(read_log(log_path)), k, -1)
    s1["scan_metrics"] = scan_metrics(read_log(log_path))
    record("s1_startup", run, s1)

    rss0 = rss_sample(proc.pid, "s2_start")
    t0 = time.monotonic()
    key("ctrl+F8")
    max_q = 0
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
    rss1 = rss_sample(proc.pid, "s2_end")
    record("s2_generate_all", run, {
        "done": done, "elapsed_s": round(elapsed, 2), "max_queue": max_q,
        "p50_ms": num(s, "p50"), "p95_ms": num(s, "p95"), "p99_ms": num(s, "p99"),
        "fail": num(s, "fail"), "cache_bytes": num(s, "cb"),
        "rss_start_kb": rss0["rss_total_kb"], "rss_end_kb": rss1["rss_total_kb"],
    })
    return cache, proc, log_path, home


def session_B(run, cache, proc, rss_detail=False, home=None):
    """SIGTERM → 再起動してキャッシュ済み再表示 (s8) → 削除→再生成 (s10)。"""
    graceful = terminate(proc)
    log_path = f"{WP16}/logs/s7s8-r{run}.log"
    open(log_path, "wb").close()
    t0 = time.monotonic()
    proc = launch(cache, log_path, db_path=f"{home}/library.sqlite3" if home else None,
                  home=home)
    boot_wall = time.monotonic() - t0
    key("F8")
    ok = wait_for(log_path, lambda s: num(s, "tot") == 1615, 30)
    ok_hit = wait_for(
        log_path, lambda s: num(s, "hr") == 1.0 and num(s, "mis") == 0 and num(s, "hit") > 0, 30)
    time.sleep(1.5)
    s = last_snap(read_log(log_path))
    rss_idle = rss_sample(proc.pid, "s8_idle")
    record("s8_cached_reload", run, {
        "graceful_prev": graceful,
        "rss_idle_kb": rss_idle["rss_total_kb"], "rss_main_kb": rss_idle["rss_main_kb"],
        "boot_wall_s": round(boot_wall, 2), "scan_ok": ok, "all_hit": ok_hit,
        "gen": num(s, "gen"), "hit": num(s, "hit"), "miss": num(s, "mis"),
        "hr": num(s, "hr", -1), "q": num(s, "q"),
        "proto_p50_ms": num(s, "pp50"), "proto_p95_ms": num(s, "pp95"),
        "dH": num(s, "dH"), "dP": num(s, "dP"),
    })
    if rss_detail:
        # RSS内訳 (受け入れ条件: 504MB前後の内訳説明) — smaps_rollupを採取
        breakdown = smaps_rollup_all(proc.pid)
        record("s8_rss_breakdown", run, breakdown)
        time.sleep(2.0)
    # s10: 削除→再生成 (スパイクは×3、回帰チェックは×1で足りる)
    key("ctrl+F9")
    time.sleep(2.0)
    base = last_snap(read_log(log_path))
    gen0, fail0 = num(base, "gen"), num(base, "fail")
    t0 = time.monotonic()
    key("ctrl+F8")
    done = False
    while time.monotonic() - t0 < 600:
        s = last_snap(read_log(log_path))
        if num(s, "q") == 0 and num(s, "gen") - gen0 + num(s, "fail") - fail0 >= 1615:
            done = True
            break
        time.sleep(0.2)
    elapsed = time.monotonic() - t0
    s = last_snap(read_log(log_path))
    record("s10_clear_regen", run, {
        "done": done, "elapsed_s": round(elapsed, 2),
        "p50_ms": num(s, "p50"), "p95_ms": num(s, "p95"), "p99_ms": num(s, "p99"),
        "fail": num(s, "fail"),
    })
    return proc, log_path


def smaps_rollup_all(pid):
    """本体+子プロセス (WebKit) のsmaps_rollupを集計 (RSS内訳の生証拠)。"""
    rows = []

    def collect(p, is_main):
        try:
            with open(f"/proc/{p}/smaps_rollup") as f:
                fields = {}
                for line in f:
                    parts = line.split(None, 2)
                    if len(parts) >= 2 and parts[0].endswith(":"):
                        try:
                            fields[parts[0][:-1]] = int(parts[1])
                        except ValueError:
                            pass
                with open(f"/proc/{p}/comm") as c:
                    comm = c.read().strip()
                rows.append({
                    "pid": p, "comm": comm, "main": is_main,
                    "rss_kb": fields.get("Rss", 0),
                    "shared_clean_kb": fields.get("Shared_Clean", 0),
                    "shared_dirty_kb": fields.get("Shared_Dirty", 0),
                    "private_clean_kb": fields.get("Private_Clean", 0),
                    "private_dirty_kb": fields.get("Private_Dirty", 0),
                    "anonymous_kb": fields.get("Anonymous", 0),
                    "swap_kb": fields.get("Swap", 0),
                })
        except OSError:
            return
        try:
            kids = open(f"/proc/{p}/task/{p}/children").read().split()
        except OSError:
            return
        for c in kids:
            collect(int(c), False)

    collect(pid, True)
    return {"processes": rows}


def session_scroll(proc, log_path):
    """S3: コールド相当 (s10後の空キャッシュ) で生成しながら高速スクロール。"""
    key("ctrl+F2")
    time.sleep(1.0)
    for it, repeat in enumerate((40, 80, 40)):
        key("Home")
        time.sleep(1.2)
        before = last_snap(read_log(log_path))
        t0 = time.monotonic()
        key("--repeat", str(repeat), "--delay", "40", "Page_Down")
        keys_wall = time.monotonic() - t0
        time.sleep(3.0)
        after = last_snap(read_log(log_path))
        record("s3_fast_scroll", it + 1, {
            "pages_down": repeat, "keys_wall_s": round(keys_wall, 2),
            "long_tasks": num(after, "lt") - num(before, "lt"),
            "long_frames": num(after, "lf") - num(before, "lf"),
            "worst_frame_ms": num(after, "lfMax"),
            "cancelled": num(after, "can") - num(before, "can"),
            "requests": num(after, "req") - num(before, "req"),
            "generated": num(after, "gen") - num(before, "gen"),
            "q_after": num(after, "q"), "dom": num(after, "dom"),
        })
        time.sleep(1.5)
        s = last_snap(read_log(log_path))
        results["s3_fast_scroll"][-1]["q_settled"] = num(s, "q")


def session_viewer(proc, log_path):
    """S4: 単一画像を連続100枚移動 ×3 (スパイクS4相当)。"""
    # s256を確保 (s10で再生済みのはず。ウォーム時はgenが増えないため
    # q==0 && 20s経過 でも打ち切り)
    t0 = time.monotonic()
    key("ctrl+F8")
    while time.monotonic() - t0 < 600:
        s = last_snap(read_log(log_path))
        if num(s, "q") == 0 and (
            num(s, "gen") + num(s, "fail") >= 1615 or time.monotonic() - t0 > 20
        ):
            break
        time.sleep(0.5)
    key("ctrl+F2")
    time.sleep(0.5)
    for it in range(3):
        key("Home"); time.sleep(1.0)
        key("F2"); time.sleep(0.3)
        key("F3")
        time.sleep(2.5)
        key("ctrl+F6")
        time.sleep(0.5)
        before = last_snap(read_log(log_path))
        t0 = time.monotonic()
        key("--repeat", "100", "--delay", "25", "Right")
        keys_wall = time.monotonic() - t0
        time.sleep(2.5)
        after = last_snap(read_log(log_path))
        record("s4_viewer_100", it + 1, {
            "keys_wall_s": round(keys_wall, 2),
            "applied": num(after, "ap"), "stale": num(after, "stale"),
            "nav_p50_ms": num(after, "onP50", -1),
            "nav_p95_ms": num(after, "onP95", -1),
            "nav_p99_ms": num(after, "onP99", -1),
            "long_frames": num(after, "lf") - num(before, "lf"),
            "worst_frame_ms": num(after, "lfMax"),
        })
        key("Escape")
        time.sleep(0.8)


def session_virtual(proc, log_path, cache):
    """S9: 5万件仮想カタログの先頭/中央/末尾ジャンプ ×3。"""
    jumps = [("middle", "ctrl+F11"), ("end", "ctrl+F12"), ("start", "ctrl+F10")]
    for it in range(3):
        key("F9")
        ok_mode = wait_for(log_path, lambda s: num(s, "tot") == 50000, 30)
        key("ctrl+F2")
        time.sleep(0.5)
        for name, k in jumps:
            before = last_snap(read_log(log_path))
            pf0 = num(before, "pf")
            t0 = time.monotonic()
            key(k)
            ok = wait_for(log_path, lambda s: num(s, "pf") > pf0 and num(s, "dom") > 0,
                          10, poll=0.1)
            elapsed = time.monotonic() - t0
            time.sleep(1.0)
            after = last_snap(read_log(log_path))
            record("s9_virtual_jump", it + 1, {
                "target": name, "mode_ok": ok_mode, "settled": ok,
                "fetch_s": round(elapsed, 3), "pages": num(after, "pf") - pf0,
                "dom": num(after, "dom"),
                "page_errors": num(after, "pe") - num(before, "pe"),
            })
        key("F10")
        wait_for(log_path, lambda s: num(s, "tot") == 1615, 30)
        time.sleep(0.8)


def main():
    ui_only = "--ui-only" in sys.argv
    if not ui_only:
        for run in (1, 2, 3):
            cache, proc, log_path, home = session_A(run)
            proc, log_path = session_B(run, cache, proc, rss_detail=(run == 3), home=home)
    # ウォームキャッシュでUI系
    cache = f"{WP16}/cache-main-r3"
    home = f"{WP16}/home-r3"
    log_path = f"{WP16}/logs/s3-s9.log"
    open(log_path, "wb").close()
    proc = launch(cache, log_path, db_path=f"{home}/library.sqlite3", home=home)
    key("F8")
    wait_for(log_path, lambda s: num(s, "tot") == 1615, 30)
    time.sleep(1.0)
    if ui_only:
        # A/Bを飛ばした再実行時: s3は省略 (既に採取済み)
        session_viewer(proc, log_path)
    else:
        session_scroll(proc, log_path)   # s10でキャッシュは再充填済み
        session_viewer(proc, log_path)
    session_virtual(proc, log_path, cache)
    rss = rss_sample(proc.pid, "after_ui_scenarios")
    results["rss_after_ui_scenarios"] = rss
    terminate(proc)
    with open(f"{WP16}/s1-results.json", "w") as f:
        json.dump(results, f, ensure_ascii=False, indent=1)
    print("S1 DONE")


if __name__ == "__main__":
    main()
