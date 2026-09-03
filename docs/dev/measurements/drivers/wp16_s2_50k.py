#!/usr/bin/env python3
"""WP-16 S2: 実5万ファイルE2E (生成コーパスで走査→DB→再起動→検索→スクロール)。

前提: scripts/wp16_gen_corpus.py で生成した corpus50k (実画像バイト混在)。
Home/DB/cacheはすべて /tmp/opencode/wp16/s2/ 配下に隔離 (実環境DB汚染防止)。

計測:
  - 初回登録→走査 (DB files行の安定で判定) →ハッシュ完了 (assets.content_hash)
  - s256全件生成 (p50/p95/p99・スループット)
  - DBサイズ (本体外+WAL)・再起動時間・catalog先頭ページ表示
  - 再起動後のキャッシュヒット率 (viewport要求)
  - キーワード検索latency (広域/狭域。S行 ms=)
  - グリッドスクロール (Page_Down 40/80/40。long frame・DOM・queue)
  - devtest行 (1sハートビート) の5秒超欠落 = UI停止検出

使い方: python3 docs/dev/measurements/drivers/wp16_s2_50k.py
"""
import json
import os
import re
import shutil
import signal
import sqlite3
import subprocess
import sys
import time

BIN = "/mnt/holyland/Project/GEN-TO/target/release/gento-tauri"
CORPUS = "/tmp/opencode/wp16/corpus50k"
WP16 = "/tmp/opencode/wp16"
WORK = f"{WP16}/s2"
ENV = dict(os.environ, DISPLAY=os.environ.get("GENTO_E2E_DISPLAY", ":6"),
           GENTO_LOG="info", GENTO_DEV_BRIDGE="1", NO_COLOR="1")

results = {"heartbeat_gaps": [], "db_poll": []}


def db_query(db, sql):
    con = sqlite3.connect(f"file:{db}?mode=ro", uri=True, timeout=5)
    try:
        return con.execute(sql).fetchone()
    finally:
        con.close()


def launch(log_path):
    log = open(log_path, "ab", buffering=0)
    env = dict(ENV, HOME=f"{WORK}/home",
               XDG_DATA_HOME=f"{WORK}/home/.local/share",
               XDG_CACHE_HOME=f"{WORK}/home/.cache")
    os.makedirs(f"{WORK}/home", exist_ok=True)
    proc = subprocess.Popen(
        [BIN, "--dataset", CORPUS, "--cache-dir", f"{WORK}/thumbs",
         "--db", f"{WORK}/home/library.sqlite3"],
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
DEVTEST_TS = re.compile(r"^(\S+)\s+Z?\s*INFO devtest:")


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


def scan_heartbeat_gaps(log_text, threshold_s=5.0):
    """devtest行 (1s周期) のタイムスタンプ欠落 > threshold をUI停止候補として記録。"""
    import datetime
    ts = []
    for line in log_text.splitlines():
        m = re.match(r"(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+)Z", line)
        if m and "devtest:" in line:
            ts.append(datetime.datetime.fromisoformat(m.group(1)))
    gaps = []
    for a, b in zip(ts, ts[1:]):
        d = (b - a).total_seconds()
        if d > threshold_s:
            gaps.append({"after": a.isoformat(), "gap_s": round(d, 1)})
    return gaps


def cache_file_count(root):
    n = 0
    for _r, _d, fs in os.walk(root):
        n += len(fs)
    return n


def phase1b_thumbs():
    """(再開用) 既存envで起動し、s256全件生成を完了させる (キュー上限再投入)。"""
    global results
    try:
        with open(f"{WORK}/results.json") as f:
            saved = json.load(f)
        results = saved
    except Exception:
        pass
    log_path = f"{WORK}/logs/phase1b.log"
    open(log_path, "wb").close()
    proc = launch(log_path)
    db = f"{WORK}/home/library.sqlite3"
    assets_n, = db_query(db, "SELECT COUNT(*) FROM assets")
    ok_tot = wait_for(log_path, lambda s: num(s, "tot") >= assets_n - 5, 120)
    base = last_snap(read_log(log_path))
    gen0 = num(base, "gen")
    t0 = time.monotonic()
    presses = 1
    key("ctrl+F8")
    max_q = 0
    last_press = t0
    while True:
        s = last_snap(read_log(log_path))
        max_q = max(max_q, num(s, "q"))
        cached = cache_file_count(f"{WORK}/thumbs")
        if cached >= assets_n:
            break
        if time.monotonic() - t0 > 5400:
            break
        if num(s, "q") == 0 and time.monotonic() - last_press > 5:
            key("ctrl+F8")
            presses += 1
            last_press = time.monotonic()
        time.sleep(1.0)
    gen_elapsed = time.monotonic() - t0
    s = last_snap(read_log(log_path))
    record("thumbs_50k", {
        "tot_ok": ok_tot, "elapsed_s": round(gen_elapsed, 1), "presses": presses,
        "cached_files": cache_file_count(f"{WORK}/thumbs"), "assets": assets_n,
        "p50_ms": num(s, "p50"), "p95_ms": num(s, "p95"), "p99_ms": num(s, "p99"),
        "fail": num(s, "fail"), "max_queue": max_q,
        "cache_bytes": num(s, "cb"),
        "rss_kb": rss_total(proc.pid),
    })
    terminate(proc)


def phase2(assets_n):
    """再起動 → 検索 → スクロール → ジャンプ → ハートビート解析。"""
    log2 = f"{WORK}/logs/phase2.log"
    open(log2, "wb").close()
    t0 = time.monotonic()
    proc = launch(log2)
    boot2 = time.monotonic() - t0
    ok_tot = wait_for(log2, lambda s: num(s, "tot") >= assets_n - 5, 120)
    t_tot = time.monotonic() - t0
    ok_hit = wait_for(log2, lambda s: num(s, "hr") >= 0.999 and num(s, "mis") == 0, 60)
    time.sleep(2)
    s = last_snap(read_log(log2))
    record("restart", {
        "boot_wall_s": round(boot2, 2),
        "to_catalog_tot_s": round(t_tot, 2), "catalog_ok": ok_tot,
        "viewport_all_hit": ok_hit, "hr": num(s, "hr", -1),
        "hit": num(s, "hit"), "miss": num(s, "mis"),
        "proto_p50_ms": num(s, "pp50"), "dH": num(s, "dH"), "dP": num(s, "dP"),
        "rss_idle_kb": rss_total(proc.pid),
        "lfMax": num(s, "lfMax"),
    })

    # キーワード検索 (広域: sunset ≈ 1/16 / 狭域: sunset_000123 / 構造化混在)
    def search(q):
        key("ctrl+shift+F12"); time.sleep(0.4); key("h"); time.sleep(0.5)
        key("--delay", "40", "type", q); time.sleep(0.3)
        key("Return")
        ok = wait_for(log2, lambda s: s.get("q") == q and num(s, "run") == 0, 60)
        time.sleep(1.0)
        s = last_snap(read_log(log2))
        return {"query": q, "ok": ok, "tot": num(s, "tot"), "ms": num(s, "ms"),
                "shown": num(s, "shown")}

    record("search_keyword", search("sunset"))
    record("search_keyword", search("sunset_000123"))
    record("search_keyword", search("cat rating:>=0"))
    key("ctrl+shift+F12"); time.sleep(0.4); key("h"); time.sleep(0.4)
    key("Escape"); time.sleep(1.0)

    # スクロール (50k実カタログ・サムネ済み)
    key("ctrl+F2")
    time.sleep(1.0)
    for it, repeat in enumerate((40, 80, 40)):
        key("Home"); time.sleep(1.5)
        before = last_snap(read_log(log2))
        t0 = time.monotonic()
        key("--repeat", str(repeat), "--delay", "40", "Page_Down")
        keys_wall = time.monotonic() - t0
        time.sleep(4.0)
        after = last_snap(read_log(log2))
        record("scroll_50k", {
            "burst": repeat, "keys_wall_s": round(keys_wall, 2),
            "long_frames": num(after, "lf") - num(before, "lf"),
            "worst_frame_ms": num(after, "lfMax"),
            "dom": num(after, "dom"), "q_after": num(after, "q"),
            "st": num(after, "st"), "pages_fetched": num(after, "pf") - num(before, "pf"),
            "cancelled": num(after, "can") - num(before, "can"),
        })
        time.sleep(1.5)

    # ジャンプ (50k実: 先頭/中央/末尾 — pager実fetch)
    for name, k in [("start", "Home"), ("middle", "ctrl+F11"), ("end", "ctrl+F12")]:
        before = last_snap(read_log(log2))
        pf0 = num(before, "pf")
        t0 = time.monotonic()
        key(k)
        ok = wait_for(log2, lambda s: num(s, "pf") > pf0 and num(s, "dom") > 0, 15, poll=0.1)
        elapsed = time.monotonic() - t0
        time.sleep(1.0)
        after = last_snap(read_log(log2))
        record("jump_50k", {
            "target": name, "settled": ok, "fetch_s": round(elapsed, 3),
            "pages": num(after, "pf") - pf0, "dom": num(after, "dom"),
        })

    terminate(proc)
    with open(f"{WORK}/results.json", "w") as f:
        json.dump(results, f, ensure_ascii=False, indent=1)
    return log2


def main():
    global results
    mode = sys.argv[1] if len(sys.argv) > 1 else ""
    os.makedirs(f"{WORK}/logs", exist_ok=True)
    db = f"{WORK}/home/library.sqlite3"

    if mode == "--phase1b":
        phase1b_thumbs()
        return
    if mode == "--phase2":
        try:
            with open(f"{WORK}/results.json") as f:
                results = json.load(f)
        except Exception:
            pass
        assets_n, = db_query(db, "SELECT COUNT(*) FROM assets")
        phase2(assets_n)
        # ハートビート解析 (直近phases)
        for phase, path in (("phase2", f"{WORK}/logs/phase2.log"),):
            gaps = scan_heartbeat_gaps(read_log(path))
            results["heartbeat_gaps"].append({"phase": phase, "gaps": gaps})
        with open(f"{WORK}/results.json", "w") as f:
            json.dump(results, f, ensure_ascii=False, indent=1)
        print("S2 PHASE2 DONE")
        return

    # ===== phase 1: 初回起動・登録・走査・ハッシュ =====
    shutil.rmtree(WORK, ignore_errors=True)
    os.makedirs(f"{WORK}/home", exist_ok=True)
    os.makedirs(f"{WORK}/logs", exist_ok=True)
    log_path = f"{WORK}/logs/phase1.log"
    open(log_path, "wb").close()
    t0 = time.monotonic()
    proc = launch(log_path)
    boot_wall = time.monotonic() - t0
    key("ctrl+shift+F12"); time.sleep(0.4); key("0")  # foAddRootDataset

    # 走査: files行が50000に達し、かつ2秒間不変で完了とみなす
    t_reg = time.monotonic()
    files_n = 0
    stable_since = None
    scan_done_at = None
    while time.monotonic() - t_reg < 900:
        try:
            files_n, = db_query(db, "SELECT COUNT(*) FROM files")
        except Exception:
            files_n = 0
        if files_n >= 50000:
            if stable_since is None:
                stable_since = time.monotonic()
            elif time.monotonic() - stable_since > 2.0:
                scan_done_at = time.monotonic()
                break
        else:
            stable_since = None
        time.sleep(1.0)
    scan_s = (scan_done_at or time.monotonic()) - t_reg if files_n else -1

    # ハッシュ: assets作成+content_hash付与が両方収束するまで
    t_hash0 = time.monotonic()
    hashed = assets_n = -1
    hash_done_at = None
    stable_since = None
    while time.monotonic() - t_hash0 < 1800:
        try:
            assets_n, = db_query(db, "SELECT COUNT(*) FROM assets")
            hashed, = db_query(db, "SELECT COUNT(*) FROM assets WHERE content_hash IS NOT NULL")
        except Exception:
            pass
        if assets_n > 0 and hashed >= assets_n and assets_n >= files_n - 2000:
            if stable_since is None:
                stable_since = time.monotonic()
            elif time.monotonic() - stable_since > 5.0:
                hash_done_at = time.monotonic()
                break
        else:
            stable_since = None
        time.sleep(2.0)
    hash_s = (hash_done_at or time.monotonic()) - t_hash0
    try:
        states = db_query(db, "SELECT scan_state, COUNT(*) FROM files GROUP BY scan_state")
    except Exception:
        states = None
    record("scan_hash", {
        "boot_wall_s": round(boot_wall, 2),
        "register_to_scan_done_s": round(scan_s, 2) if scan_s > 0 else None,
        "hash_done_s": round(hash_s, 2),
        "files": files_n, "assets": assets_n, "hashed": hashed,
        "scan_states": states,
        "rss_kb": rss_total(proc.pid),
    })

    # s256全件 (キュー上限4096のため再投入) — phase1bで再開可能
    phase1b_assets = assets_n
    base = last_snap(read_log(log_path))
    gen0 = num(base, "gen")
    t0 = time.monotonic()
    presses = 1
    key("ctrl+F8")
    max_q = 0
    refill_start = None
    while time.monotonic() - t0 < 5400:
        s = last_snap(read_log(log_path))
        max_q = max(max_q, num(s, "q"))
        cached = cache_file_count(f"{WORK}/thumbs")
        if cached >= phase1b_assets:
            break
        if num(s, "q") == 0:
            if refill_start is None:
                refill_start = time.monotonic()
                results["findings"] = results.get("findings", []) + [
                    "generate_all_thumbnails: 1回の投入はプールキュー(4096)分のみ。"
                    "5万件では再投入が必要 (全件生成ボタンのスケール制限)"]
            key("ctrl+F8")
            presses += 1
            time.sleep(2.0)
            continue
        time.sleep(1.0)
    gen_elapsed = time.monotonic() - t0
    s = last_snap(read_log(log_path))
    record("thumbs_50k", {
        "elapsed_s": round(gen_elapsed, 1), "presses": presses,
        "cached_files": cache_file_count(f"{WORK}/thumbs"), "assets": phase1b_assets,
        "p50_ms": num(s, "p50"), "p95_ms": num(s, "p95"), "p99_ms": num(s, "p99"),
        "fail": num(s, "fail"), "max_queue": max_q,
        "cache_bytes": num(s, "cb"),
        "rss_kb": rss_total(proc.pid),
    })

    # DBサイズ (稼働中 = WAL込み)
    def fsize(p):
        try:
            return os.path.getsize(p)
        except OSError:
            return 0
    record("db_size_live", {
        "main_mb": round(fsize(db) / 1e6, 1),
        "wal_mb": round(fsize(db + "-wal") / 1e6, 1),
        "shm_mb": round(fsize(db + "-shm") / 1e6, 1),
        "thumbs_mb": round(sum(
            fsize(os.path.join(r, f))
            for r, _d, fs in os.walk(f"{WORK}/thumbs") for f in fs) / 1e6, 1),
    })
    terminate(proc)

    phase2(assets_n)
    for phase, path in (("phase1", log_path), ("phase2", f"{WORK}/logs/phase2.log")):
        gaps = scan_heartbeat_gaps(read_log(path))
        results["heartbeat_gaps"].append({"phase": phase, "gaps": gaps})
    with open(f"{WORK}/results.json", "w") as f:
        json.dump(results, f, ensure_ascii=False, indent=1)
    print("S2 DONE")


if __name__ == "__main__":
    main()
