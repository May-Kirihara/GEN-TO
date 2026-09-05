#!/usr/bin/env python3
"""WP-7 フェーズ9 計測ドライバ (release build + GENTO_DEV_BRIDGE=1)。

シナリオ (docs/implementation-plan.md §16):
  S1 空キャッシュ起動 ×3 / S2 全件生成 ×3 (Session A: キャッシュ Fresh)
  S7 SIGTERM終了 ×3 / S8 キャッシュ済み再表示 ×3 / S10 削除→再生成 ×3 (Session B)
  S3 高速スクロール / S4 100枚移動 / S5 2枚比較 / S6 4枚入れ替え / S9 5万件ジャンプ
  (Session C: ウォームキャッシュ, 各×3)

値は devtest: ログ行 (1秒周期) の累積カウンタ差分とRustログから採取する。
結果は /tmp/opencode/wp7/results.json へ追記。
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
WP7 = "/tmp/opencode/wp7"
ENV = dict(os.environ, DISPLAY=":1", GENTO_LOG="info", GENTO_DEV_BRIDGE="1", NO_COLOR="1")

results = {}


def launch(cache_dir, log_path):
    log = open(log_path, "ab", buffering=0)
    proc = subprocess.Popen(
        [BIN, "--dataset", DATASET, "--cache-dir", cache_dir],
        stdout=log, stderr=subprocess.STDOUT, env=ENV, cwd=WP7,
    )
    # ウィンドウ出現待ち
    for _ in range(100):
        r = subprocess.run(["xdotool", "search", "--onlyvisible", "--pid", str(proc.pid)],
                           capture_output=True, text=True)
        if r.stdout.strip():
            break
        time.sleep(0.2)
    time.sleep(1.5)
    wids = r.stdout.strip().split("\n") if r.stdout.strip() else []
    if wids:
        subprocess.run(["xdotool", "windowactivate", "--sync", wids[0]], capture_output=True)
    return proc


def key(*args):
    subprocess.run(["xdotool", "key", *args], capture_output=True)


def shot(name):
    subprocess.run(["import", "-window", "root", f"{WP7}/shots/{name}.png"], capture_output=True)


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
SCAN_METRICS = re.compile(r"scan metrics.*found=(\d+).*scan_elapsed_ms=(\d+).*probe_elapsed_ms=(\d+).*rss_kb=(\d+)")


def read_log(path):
    with open(path, "rb") as f:
        return f.read().decode("utf-8", errors="replace")


def last_snap(log_text):
    """直近のdevtest行を {key: value} へパースする (数値はfloat化)。"""
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


def wait_for(log_path, pred, timeout=60, poll=0.25):
    """devtest行が条件を満たすまで待つ。"""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if pred(last_snap(read_log(log_path))):
            return True
        time.sleep(poll)
    return False


def wait_sigterm(proc, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            return True
        time.sleep(0.1)
    return False


def scan_metrics(log_text):
    rows = SCAN_METRICS.findall(ANSI.sub("", log_text))
    return [{"found": int(r[0]), "scan_ms": int(r[1]), "probe_ms": int(r[2]), "rss_kb": int(r[3])} for r in rows]


def num(snap, k, default=0.0):
    v = snap.get(k, default)
    return v if isinstance(v, (int, float)) else default


def record(scenario, run, data):
    results.setdefault(scenario, []).append({"run": run, **data})
    print(f"[{scenario} run{run}] {data}", flush=True)
    with open(f"{WP7}/results.json", "w") as f:
        json.dump(results, f, ensure_ascii=False, indent=1)


def count_tmp(cache):
    n = 0
    for root, _dirs, files in os.walk(cache):
        n += sum(1 for f in files if f.startswith(".tmp-"))
    return n


def terminate(proc, label):
    proc.send_signal(signal.SIGTERM)
    ok = wait_sigterm(proc, 30)
    if not ok:
        proc.kill()
    return ok


def session_A(run):
    """S1 (空キャッシュ起動) + S2 (全件生成)。戻り値: cache path"""
    cache = f"{WP7}/cache-main-r{run}"
    shutil.rmtree(cache, ignore_errors=True)
    log_path = f"{WP7}/logs/s1s2-r{run}.log"
    open(log_path, "wb").close()
    t0 = time.monotonic()
    proc = launch(cache, log_path)
    boot_wall = time.monotonic() - t0
    key("F8")  # スキャン
    ok = wait_for(log_path, lambda s: num(s, "tot") == 1615, 30)
    s1 = {"boot_wall_s": round(boot_wall, 2), "scan_ok": ok}
    for k in ("dH", "dP", "mnt", "lfMax"):
        s1[k] = num(last_snap(read_log(log_path)), k, -1)
    s1["scan_metrics"] = scan_metrics(read_log(log_path))
    record("s1_startup", run, s1)

    # S2: 全件生成
    rss0 = rss_sample(proc.pid, "s2_start")
    t0 = time.monotonic()
    key("ctrl+F8")
    max_q = 0
    done = False
    while time.monotonic() - t0 < 300:
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
    if run == 3:
        shot("s2_grid_generated")
    return cache, proc, log_path


def session_B(run, cache, proc):
    """S7 (SIGTERM) は session_A の proc を終了させるところから。"""
    t0 = time.monotonic()
    graceful = terminate(proc, f"A-r{run}")
    record("s7_sigterm", run, {
        "graceful": graceful, "tmp_files": count_tmp(cache),
        "exit_wall_s": round(time.monotonic() - t0, 2),
    })
    # S8: 再起動 → キャッシュ済み再表示
    log_path = f"{WP7}/logs/s7s8-r{run}.log"
    open(log_path, "wb").close()
    t0 = time.monotonic()
    proc = launch(cache, log_path)
    boot_wall = time.monotonic() - t0
    key("F8")
    ok = wait_for(log_path, lambda s: num(s, "tot") == 1615, 30)
    # グリッド需要 (ビューポート) が全件ヒットするまで待つ (統計ポーリング1sに合わせる)
    ok_hit = wait_for(log_path, lambda s: num(s, "hr") == 1.0 and num(s, "mis") == 0 and num(s, "hit") > 0, 30)
    time.sleep(1.5)
    s = last_snap(read_log(log_path))
    rss_idle = rss_sample(proc.pid, "s8_idle")
    record("s8_cached_reload", run, {
        "rss_idle_kb": rss_idle["rss_total_kb"],
        "boot_wall_s": round(boot_wall, 2), "scan_ok": ok, "all_hit": ok_hit,
        "gen": num(s, "gen"), "hit": num(s, "hit"), "miss": num(s, "mis"),
        "hr": num(s, "hr", -1), "q": num(s, "q"),
        "proto_p50_ms": num(s, "pp50"), "proto_p95_ms": num(s, "pp95"),
        "dH": num(s, "dH"), "dP": num(s, "dP"),
    })
    if run == 3:
        shot("s8_cached_grid")
    # S10: キャッシュ削除 → 再生成
    key("ctrl+F9")
    time.sleep(2.0)
    base = last_snap(read_log(log_path))
    gen0, fail0 = num(base, "gen"), num(base, "fail")
    t0 = time.monotonic()
    key("ctrl+F8")
    done = False
    while time.monotonic() - t0 < 300:
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
    terminate(proc, f"B-r{run}")
    return


def session_C_cold():
    """S3: コールドキャッシュで生成しながら高速スクロール (§16「生成中に」)。"""
    cache = f"{WP7}/cache-cold"
    shutil.rmtree(cache, ignore_errors=True)
    log_path = f"{WP7}/logs/s3-cold.log"
    open(log_path, "wb").close()
    proc = launch(cache, log_path)
    key("F8")
    wait_for(log_path, lambda s: num(s, "tot") == 1615, 30)
    key("ctrl+F2")  # グリッドへフォーカス
    time.sleep(1.0)
    for it, repeat in enumerate((40, 80, 40)):
        key("Home")
        time.sleep(1.2)
        before = last_snap(read_log(log_path))
        t0 = time.monotonic()
        key("--repeat", str(repeat), "--delay", "40", "Page_Down")
        keys_wall = time.monotonic() - t0
        time.sleep(3.0)  # 収束待ち
        after = last_snap(read_log(log_path))
        record("s3_fast_scroll", it + 1, {
            "pages_down": repeat, "keys_wall_s": round(keys_wall, 2),
            "long_tasks": num(after, "lt") - num(before, "lt"),
            "long_frames": num(after, "lf") - num(before, "lf"),
            "worst_frame_ms": num(after, "lfMax"),
            "cancelled": num(after, "can") - num(before, "can"),
            "cancel_batches": num(after, "canB") - num(before, "canB"),
            "requests": num(after, "req") - num(before, "req"),
            "ready_recv": num(after, "rdy") - num(before, "rdy"),
            "generated": num(after, "gen") - num(before, "gen"),
            "q_after": num(after, "q"), "dom": num(after, "dom"),
        })
        time.sleep(1.5)
        s = last_snap(read_log(log_path))
        results["s3_fast_scroll"][-1]["q_settled"] = num(s, "q")
    shot("s3_after_scroll")
    terminate(proc, "C-cold")


def session_C_warm():
    """S4/S5/S6: ウォームキャッシュ (s256+preview2048) でのビューア系シナリオ。"""
    cache = f"{WP7}/cache-main-r3"
    log_path = f"{WP7}/logs/s4-s6.log"
    open(log_path, "wb").close()
    proc = launch(cache, log_path)
    key("F8")
    wait_for(log_path, lambda s: num(s, "tot") == 1615, 30)
    time.sleep(1.0)
    base = last_snap(read_log(log_path))
    gen0 = num(base, "gen")
    t0 = time.monotonic()
    key("shift+F8")  # preview2048 全件生成
    # q>0 が出て q==0 に戻り gen が 1615 増えるのを待つ
    started = False
    while time.monotonic() - t0 < 900:
        s = last_snap(read_log(log_path))
        if num(s, "q") > 0:
            started = True
        if started and num(s, "q") == 0 and num(s, "gen") - gen0 >= 1615:
            break
        if not started and time.monotonic() - t0 > 8 and num(s, "gen") == gen0:
            # 全件キャッシュ済み (前回実行で生成済み): ヒット確認後に進む
            time.sleep(2.0)
            break
        time.sleep(1.0)
    pv = last_snap(read_log(log_path))
    record("s456_preview_warmup", 1, {
        "elapsed_s": round(time.monotonic() - t0, 1), "previews": num(pv, "gen") - gen0,
        "p50_ms": num(pv, "p50"), "p95_ms": num(pv, "p95"), "p99_ms": num(pv, "p99"),
    })

    # S4: 単一画像を連続100枚移動 ×3 (X keysymは Right = DOM ArrowRight)
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
            "prefetch": num(after, "pre", -1),
            "navN": num(after, "onN"),
            "nav_p50_ms": num(after, "onP50", -1),
            "nav_p95_ms": num(after, "onP95", -1),
            "nav_p99_ms": num(after, "onP99", -1),
            "long_frames": num(after, "lf") - num(before, "lf"),
            "worst_frame_ms": num(after, "lfMax"),
        })
        key("Escape")
        time.sleep(0.8)
    shot("s4_viewer")

    # S5: 2枚比較で同期ズーム・パン ×3
    key("F7")
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
        # UIUX-3: 比較ビューのwheelズームは不変 (F4素押し=slot0へ平常wheel)。
        # 単一ビューアでのズームはvwCmdMode (素F12) のi/oを使う。
        key("F4")
        time.sleep(0.8)
        key("F5")
        time.sleep(3.0)
        after = last_snap(read_log(log_path))
        record("s5_compare2_sync", it + 1, {
            "frames": num(after, "frN", -1),
            "fr_p50_ms": num(after, "frP50", -1),
            "fr_p95_ms": num(after, "frP95", -1),
            "fr_max_ms": num(after, "frMax", -1),
            "sync_c": after.get("c", None), "zoom": after.get("zf", None),
            "long_frames": num(after, "lf") - num(before, "lf"),
        })
        key("Escape")
        time.sleep(0.8)
    shot("s5_compare2")

    # S6: 4枚比較で連続入れ替え ×3 (g ×10)
    key("F7")
    time.sleep(0.5)
    key("ctrl+F2")
    time.sleep(0.5)
    for it in range(3):
        key("Home"); time.sleep(1.0)
        key("ctrl+F3"); time.sleep(0.3)
        key("F3")
        time.sleep(3.0)
        key("ctrl+F6")
        time.sleep(0.5)
        before = last_snap(read_log(log_path))
        t0 = time.monotonic()
        key("--repeat", "10", "--delay", "300", "g")
        keys_wall = time.monotonic() - t0
        time.sleep(2.0)
        after = last_snap(read_log(log_path))
        record("s6_compare4_swap", it + 1, {
            "keys_wall_s": round(keys_wall, 2),
            "groups_before": num(before, "grp"),
            "groups": num(after, "grp"),
            "long_frames": num(after, "lf") - num(before, "lf"),
        })
        # 4枚表示のパンフレームレート (同期ドラッグ24ステップ)
        key("ctrl+F6")
        time.sleep(0.5)
        before2 = last_snap(read_log(log_path))
        key("F5")
        time.sleep(3.0)
        after2 = last_snap(read_log(log_path))
        results["s6_compare4_swap"][-1].update({
            "frames": num(after2, "frN", -1),
            "fr_p50_ms": num(after2, "frP50", -1),
            "fr_p95_ms": num(after2, "frP95", -1),
            "fr_max_ms": num(after2, "frMax", -1),
            "long_frames_drag": num(after2, "lf") - num(before2, "lf"),
        })
        key("Escape")
        time.sleep(0.8)
    shot("s6_compare4")
    rss = rss_sample(proc.pid, "after_ui_scenarios")
    results["rss_after_ui_scenarios"] = rss
    terminate(proc, "C-warm")


def session_C_virtual():
    """S9: 5万件仮想カタログの先頭/中央/末尾ジャンプ ×3 (モード再切替で毎回コールド)。"""
    cache = f"{WP7}/cache-main-r3"
    log_path = f"{WP7}/logs/s9.log"
    open(log_path, "wb").close()
    proc = launch(cache, log_path)
    key("F8")
    wait_for(log_path, lambda s: num(s, "tot") == 1615, 30)
    # マウント位置のページは既に取得済みになるため、中/末尾を先に測る
    jumps = [("middle", "ctrl+F11"), ("end", "ctrl+F12"), ("start", "ctrl+F10")]
    for it in range(3):
        # 実⇔仮想を切り替えてpagerをリセットし毎回コールドで計測
        key("F9")
        ok_mode = wait_for(log_path, lambda s: num(s, "tot") == 50000, 30)
        key("ctrl+F2")
        time.sleep(0.5)
        for name, k in jumps:
            before = last_snap(read_log(log_path))
            pf0 = num(before, "pf")
            t0 = time.monotonic()
            key(k)
            ok = wait_for(log_path, lambda s: num(s, "pf") > pf0 and num(s, "dom") > 0, 10, poll=0.1)
            elapsed = time.monotonic() - t0
            time.sleep(1.0)
            after = last_snap(read_log(log_path))
            record("s9_virtual_jump", it + 1, {
                "target": name, "mode_ok": ok_mode, "settled": ok,
                "fetch_s": round(elapsed, 3), "pages": num(after, "pf") - pf0,
                "dom": num(after, "dom"),
                "page_errors": num(after, "pe") - num(before, "pe"),
            })
        key("F10")  # 実カタログへ戻す
        wait_for(log_path, lambda s: num(s, "tot") == 1615, 30)
        time.sleep(0.8)
    shot("s9_virtual_end")
    terminate(proc, "C-virtual")


def main():
    for run in (1, 2, 3):
        cache, proc, _ = session_A(run)
        session_B(run, cache, proc)
    session_C_cold()
    session_C_warm()
    session_C_virtual()
    with open(f"{WP7}/results.json", "w") as f:
        json.dump(results, f, ensure_ascii=False, indent=1)
    print("ALL DONE")


if __name__ == "__main__":
    main()
