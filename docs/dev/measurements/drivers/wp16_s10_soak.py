#!/usr/bin/env python3
"""WP-16 S10: 長時間連続運転 (soak ≤45分) — チャーン+計測ローテーション。

corpus1kライブラリで40分間、以下をローテーション:
  - ファイルチャーン: 追加 (poolからコピー) / 改名 / 削除 (外部rm)
  - 走査はwatcher+定期再走査に任せる (明示F8はしない)
  - 検索・スクロール・(時々)ビューア操作
監視: RSS (30s毎)・fd数 (/proc/PID/fd)・devtestハートビート・long frame。
結果: /tmp/opencode/wp16/s10/results.json

使い方: python3 docs/dev/measurements/drivers/wp16_s10_soak.py [分数]
"""
import json
import os
import random
import re
import shutil
import signal
import subprocess
import sys
import threading
import time

BIN = "/mnt/holyland/Project/GEN-TO/target/release/gento-tauri"
CORPUS1K = "/tmp/opencode/wp16/corpus1k"
POOL = "/tmp/opencode/wp16/soak-pool"   # 追加用画像プール (起動前準備)
WP16 = "/tmp/opoly" if False else "/tmp/opencode/wp16"
WORK = f"{WP16}/s10"
ENV = dict(os.environ, DISPLAY=os.environ.get("GENTO_E2E_DISPLAY", ":6"),
           GENTO_LOG="info", GENTO_DEV_BRIDGE="1", NO_COLOR="1")
MINUTES = int(sys.argv[1]) if len(sys.argv) > 1 else 40

results = {"rss_series": [], "fd_series": [], "churn_events": [], "heartbeats": 0}
ANSI = re.compile(r"\x1b\[[0-9;]*m")


def key(*args):
    subprocess.run(["xdotool", "key", *args], capture_output=True, env=ENV)


def snap(log_path):
    txt = open(log_path, errors="replace").read()
    last = None
    for line in reversed(txt.splitlines()[-6:]):
        if "devtest:" in line:
            last = line
            break
    if not last:
        return {}
    d = {}
    for t in re.finditer(r"([A-Za-z0-9_]+)=([^\s|]+)", last.split("devtest:", 1)[1]):
        k = t.group(1)
        if k in d:
            continue
        try:
            d[k] = float(t.group(2))
        except ValueError:
            d[k] = t.group(2)
    return d


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


def fd_count(pid):
    try:
        return len(os.listdir(f"/proc/{pid}/fd"))
    except OSError:
        return -1


def main():
    import sqlite3
    shutil.rmtree(WORK, ignore_errors=True)
    os.makedirs(f"{WORK}/home", exist_ok=True)
    os.makedirs(f"{WORK}/logs", exist_ok=True)
    # チャーン用プール (追加対象の新規画像 400枚)
    if not os.path.isdir(POOL):
        subprocess.run(["python3",
                        "/mnt/holyland/Project/GEN-TO/scripts/wp16_gen_corpus.py",
                        POOL, "400"], check=True)

    log_path = f"{WORK}/logs/app.log"
    open(log_path, "wb").close()
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

    # 登録+初回ハッシュ+ウォームまで待つ (マウント確認後に登録)
    def _snap():
        return snap(log_path)
    _t = time.monotonic()
    while time.monotonic() - _t < 60:
        if _snap().get("tot", 0) > 0:
            break
        time.sleep(0.5)
    time.sleep(1.5)
    key("ctrl+shift+F12"); time.sleep(0.5); key("0")
    key("ctrl+shift+F12"); time.sleep(0.4); key("1")  # library行
    t0 = time.time()
    while time.time() - t0 < 300:
        try:
            con = sqlite3.connect(f"file:{WORK}/home/library.sqlite3?mode=ro", uri=True)
            assets = con.execute("SELECT COUNT(*) FROM assets").fetchone()[0]
            con.close()
        except Exception:
            assets = 0
        if assets >= 800:
            break
        time.sleep(2)
    key("ctrl+shift+F12"); time.sleep(0.4); key("1")  # library行
    time.sleep(5)
    results["initial"] = {"assets": assets, "warmup_s": round(time.time() - t0, 1),
                          "rss_kb": rss_total(proc.pid), "fd": fd_count(proc.pid)}

    rng = random.Random(20260903)
    # プールは「ファイル」のみ対象 (ジェネレータ出力はdirサブディレクトリを持つ)
    pool_files = sorted(
        f for f in os.listdir(POOL)
        if os.path.isfile(os.path.join(POOL, f)) and ".gento-tmp" not in f
    )
    if not pool_files:
        # dir000形式レイアウトの場合はその中身を使う
        for sub in sorted(os.listdir(POOL)):
            p = os.path.join(POOL, sub)
            if os.path.isdir(p):
                pool_files += [f"{sub}/{f}" for f in sorted(os.listdir(p))]
    pool_i = 0
    added = renamed = deleted = 0
    deadline = time.monotonic() + MINUTES * 60
    last_sample = 0.0
    last_churn = 0.0
    last_ui = 0.0
    cycle = 0
    churn_errors = []

    while time.monotonic() < deadline:
        now = time.monotonic()
        # 30s毎のサンプリング
        if now - last_sample >= 30:
            last_sample = now
            s = snap(log_path)
            results["rss_series"].append({
                "t": round(now - t0), "rss_kb": rss_total(proc.pid),
                "fd": fd_count(proc.pid), "lf": s.get("lf"), "lfMax": s.get("lfMax"),
                "tot": s.get("tot"), "q": s.get("q"), "hr": s.get("hr")})
        # 20s毎のチャーン
        if now - last_churn >= 20:
            last_churn = now
            cycle += 1
            try:
                # 追加 ×3 (dstはファイル名のみ — プール側サブディレクトリ名を除く)
                for _ in range(3):
                    if pool_i >= len(pool_files):
                        break
                    src = os.path.join(POOL, pool_files[pool_i])
                    base = os.path.basename(pool_files[pool_i])
                    dst = os.path.join(CORPUS1K, f"soak_{pool_i:05d}_{base}")
                    shutil.copy2(src, dst)
                    pool_i += 1
                    added += 1
                # 改名 ×2 (soakファイル優先、無ければ既存)
                soak_files = [f for f in os.listdir(CORPUS1K) if f.startswith("soak_")]
                for f in soak_files[:2]:
                    base, ext = os.path.splitext(f)
                    os.rename(os.path.join(CORPUS1K, f),
                              os.path.join(CORPUS1K, base + "_r" + ext))
                    renamed += 1
                # 削除 ×2 (改名済みsoakファイルを優先 — 追加分で Rollback 可能に)
                for f in [x for x in os.listdir(CORPUS1K) if "_r." in x][:2]:
                    os.remove(os.path.join(CORPUS1K, f))
                    deleted += 1
                results["churn_events"].append(
                    {"cycle": cycle, "t": round(now - t0),
                     "added": added, "renamed": renamed, "deleted": deleted})
            except OSError as e:
                churn_errors.append(str(e))
        # 25s毎のUI操作ローテーション (検索→スクロール→検索…)
        if now - last_ui >= 25:
            last_ui = now
            mode = cycle % 3
            if mode == 0:
                key("ctrl+shift+F12"); time.sleep(0.4); key("h"); time.sleep(0.5)
                subprocess.run(["xdotool", "type", "--delay", "40", "sunset"],
                               capture_output=True, env=ENV); time.sleep(0.3)
                key("Return"); time.sleep(1.5)
            elif mode == 1:
                key("--repeat", str(10 + cycle % 20), "--delay", "40", "Page_Down")
            else:
                key("Home")
            time.sleep(0.5)
        if proc.poll() is not None:
            results["FATAL"] = "app exited during soak!"
            break
        time.sleep(1.0)

    # 終了状態
    s = snap(log_path)
    results["final"] = {
        "rss_kb": rss_total(proc.pid), "fd": fd_count(proc.pid),
        "lf": s.get("lf"), "lfMax": s.get("lfMax"), "tot": s.get("tot"),
        "hr": s.get("hr"), "churn_errors": churn_errors[:5],
        "elapsed_min": round((time.monotonic() - (deadline - MINUTES * 60)) / 60, 1),
    }
    proc.send_signal(signal.SIGTERM)
    try:
        proc.wait(timeout=30)
    except Exception:
        proc.kill()
    with open(f"{WORK}/results.json", "w") as f:
        json.dump(results, f, ensure_ascii=False, indent=1)
    print(json.dumps({"initial": results["initial"], "final": results["final"],
                      "samples": len(results["rss_series"]),
                      "churn_cycles": len(results["churn_events"])}, ensure_ascii=False))


if __name__ == "__main__":
    main()
