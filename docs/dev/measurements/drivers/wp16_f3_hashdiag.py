#!/usr/bin/env python3
"""WP-16 F3: 50kハッシュ低速帯の診断 (小規模 5-10k での再計測)。

背景 (wp16-baselines.md §12-2): 50k初回投入で初段 ~6件/s × 11分の低速帯が
発生 (microbench 0.6-14ms/件と乖離) し、WAL 252MB膨張と同時発生、その後
~77件/sへ回復した。旧実行ログ (s2/logs/phase1.log) の分析メモ:
  - 低速帯中もサムネイル生成は未稼働 (gen=30固定) → サムネIL競合説は弱い
  - 同期間に "fts index repaired after scan rebuilt=0 removed=0" が
    ~6/s (計3,918回) 記録 → fts_countsの2 COUNTが別snapshotで走る
    (writer commitを跨ぐrace) により、ハッシュ中は常に「不整合」と判定
    → ensure_fts_after_scanがwriter権限で無駄なfts_repairトランザクション
    を繰り返した形跡

本ドライバは 1s周期で (hashed, WAL/mainサイズ, /proc IO, CPU, fts repair
ログ件数) を採取し、低速帯の再現・相関を特定する。終了時にSIGTERMで
graceful shutdown → WAL 0バイト (WP-16 F2修正の実機確認) も併せて採る。

使い方:
  python3 docs/dev/measurements/drivers/wp16_f3_hashdiag.py [件数]
    (コーパスは scripts/wp16_gen_corpus.py で <件数> 件を生成済みなら再利用)
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
GEN = "/mnt/holyland/Project/GEN-TO/scripts/wp16_gen_corpus.py"
WP16 = "/tmp/opencode/wp16"
N = int(sys.argv[1]) if len(sys.argv) > 1 else 8000
WORK = f"{WP16}/f3"
# 既存コーパス (例: 50k再計測) は F3_CORPUS=<dir> で直接指定できる。
CORPUS = os.environ.get("F3_CORPUS") or f"{WORK}/corpus{N}"
ENV = dict(os.environ, DISPLAY=os.environ.get("GENTO_E2E_DISPLAY", ":6"),
           GENTO_LOG="info", GENTO_DEV_BRIDGE="1", NO_COLOR="1")

results = {"n": N, "timeline": [], "phases": {}}


def db_query(db, sql):
    con = sqlite3.connect(f"file:{db}?mode=ro", uri=True, timeout=5)
    try:
        return con.execute(sql).fetchone()
    finally:
        con.close()


def fsize(p):
    try:
        return os.path.getsize(p)
    except OSError:
        return 0


def proc_io(pid):
    """read_bytes/write_bytes (実IO) と CPU時間 (utime+stime, jiffies)。"""
    io = {}
    try:
        with open(f"/proc/{pid}/io") as f:
            for line in f:
                k, v = line.split(":")
                io[k.strip()] = int(v.strip())
    except OSError:
        pass
    cpu = 0.0
    try:
        with open(f"/proc/{pid}/stat") as f:
            parts = f.read().rsplit(") ", 1)[1].split()
        cpu = (int(parts[11]) + int(parts[12])) / os.sysconf("SC_CLK_TCK")
    except (OSError, IndexError, ValueError):
        pass
    return io.get("read_bytes", 0), io.get("write_bytes", 0), cpu


def count_log(log_path, pattern):
    n = 0
    try:
        with open(log_path, "rb") as f:
            for raw in f:
                if pattern in raw:
                    n += 1
    except OSError:
        pass
    return n


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
    return proc


def focus_app():
    """アプリへキーボードフォーカスを当てる。WM無しXephyrではEWMHの
    windowactivateが機能しないためXSetInputFocus (windowfocus) を使う
    (--class検索。--pid検索は別窓を掴み得る)。--syncはフォーカス確認が
    永戻しない環境でハングするため使わない。"""
    r = subprocess.run(["xdotool", "search", "--onlyvisible", "--class", "gento-tauri"],
                       capture_output=True, text=True, env=ENV).stdout.strip()
    if r:
        subprocess.run(["xdotool", "windowfocus", r.split("\n")[0]],
                       capture_output=True, env=ENV)


def register_root_with_retry(db):
    """ctrl+shift+F12 → 0 (foAddRootDataset) を送り、登録成否をDBで確認。
    失敗時 (フォーカス負け等) は再送する。"""
    for attempt in range(10):
        focus_app()
        time.sleep(0.3)
        key("ctrl+shift+F12")
        time.sleep(0.4)
        key("0")
        time.sleep(2.0)
        try:
            roots, = db_query(db, "SELECT COUNT(*) FROM library_roots")
        except Exception:
            roots = 0
        if roots > 0:
            print(f"library root registered (attempt {attempt + 1})", flush=True)
            return True
    return False


def key(*args):
    subprocess.run(["xdotool", "key", *args], capture_output=True, env=ENV)


def terminate(proc):
    proc.send_signal(signal.SIGTERM)
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            return True
        time.sleep(0.1)
    proc.kill()
    return False


def ensure_corpus():
    if not os.path.isdir(CORPUS) or len(os.listdir(CORPUS)) == 0:
        print(f"generating corpus {CORPUS} ({N} files) ...", flush=True)
        subprocess.run([sys.executable, GEN, CORPUS, str(N)], check=True)
    n = sum(len(fs) for _r, _d, fs in os.walk(CORPUS))
    print(f"corpus ready: {n} files", flush=True)
    return n


def main():
    corpus_files = ensure_corpus()
    shutil.rmtree(f"{WORK}/home", ignore_errors=True)
    shutil.rmtree(f"{WORK}/thumbs", ignore_errors=True)
    os.makedirs(f"{WORK}/logs", exist_ok=True)
    db = f"{WORK}/home/library.sqlite3"
    log_path = f"{WORK}/logs/f3.log"
    open(log_path, "wb").close()

    t0 = time.monotonic()
    proc = launch(log_path)
    boot_wall = time.monotonic() - t0
    # ウィンドウ出現を待ってから登録 (WebViewerの準備前にキーを送っても
    # 無視されることがあるため、余裕を持って待つ)
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        r = subprocess.run(["xdotool", "search", "--onlyvisible", "--class", "gento-tauri"],
                           capture_output=True, text=True, env=ENV).stdout.strip()
        if r:
            break
        time.sleep(0.2)
    time.sleep(3.0)
    if not register_root_with_retry(db):
        print("FATAL: could not register library root (keys not reaching app?)",
              flush=True)
        terminate(proc)
        sys.exit(2)

    # 1s周期サンプリング: 走査→ハッシュ完了 (安定5s) まで
    reg_t = time.monotonic()
    files_n = assets_n = hashed = 0
    stable_since = None
    prev_r = prev_w = prev_cpu = 0
    prev_hashed = 0
    prev_t = reg_t
    while time.monotonic() - reg_t < 2400:
        try:
            files_n, = db_query(db, "SELECT COUNT(*) FROM files")
            assets_n, = db_query(db, "SELECT COUNT(*) FROM assets")
            hashed, = db_query(db,
                               "SELECT COUNT(*) FROM assets WHERE content_hash IS NOT NULL")
        except Exception:
            pass
        now = time.monotonic()
        rb, wb, cpu = proc_io(proc.pid)
        row = {
            "t": round(now - reg_t, 1),
            "files": files_n, "assets": assets_n, "hashed": hashed,
            "rate": round((hashed - prev_hashed) / max(0.001, now - prev_t), 1),
            "wal_mb": round(fsize(db + "-wal") / 1e6, 2),
            "main_mb": round(fsize(db) / 1e6, 2),
            "read_mb": round((rb - prev_r) / 1e6, 1),
            "write_mb": round((wb - prev_w) / 1e6, 1),
            "cpu_s": round(cpu, 1),
            "fts_repair": count_log(log_path, b"fts index repaired"),
        }
        results["timeline"].append(row)
        if len(results["timeline"]) % 30 == 0:
            print(f"[t={row['t']:7.1f}s] hashed={hashed:6d} rate={row['rate']:6.1f}/s "
                  f"wal={row['wal_mb']:8.2f}MB main={row['main_mb']:6.1f}MB "
                  f"fts={row['fts_repair']}", flush=True)
        prev_r, prev_w, prev_cpu, prev_hashed, prev_t = rb, wb, cpu, hashed, now

        # corpusの~1.7%は内容重複 (asset統合) ・破損扱いのため、実assets数は
        # filesより少ない。files対比で判定 (s2 driverと同じ趣向)。
        if (files_n > 0 and assets_n > 0 and hashed >= assets_n
                and assets_n >= files_n - max(150, files_n // 20)):
            if stable_since is None:
                stable_since = now
            elif now - stable_since > 5.0:
                break
        else:
            stable_since = None
        time.sleep(1.0)

    hash_elapsed = time.monotonic() - reg_t
    results["phases"]["register_to_hash_done_s"] = round(hash_elapsed, 1)
    results["phases"]["files"] = files_n
    results["phases"]["assets"] = assets_n
    results["phases"]["hashed"] = hashed
    results["phases"]["boot_wall_s"] = round(boot_wall, 2)
    results["phases"]["fts_repair_total"] = count_log(log_path, b"fts index repaired")

    # WP-16 F2 実機確認: graceful SIGTERM → WALが0バイトに切り詰められるか
    results["phases"]["wal_before_exit_mb"] = round(fsize(db + "-wal") / 1e6, 2)
    graceful = terminate(proc)
    time.sleep(0.5)
    results["phases"]["graceful_exit"] = graceful
    results["phases"]["wal_after_exit_b"] = fsize(db + "-wal")
    results["phases"]["main_after_exit_mb"] = round(fsize(db) / 1e6, 2)
    # DB整合性
    try:
        ok, = db_query(db, "PRAGMA quick_check")
        results["phases"]["quick_check_after_exit"] = ok
    except Exception as e:
        results["phases"]["quick_check_after_exit"] = f"error: {e}"

    with open(f"{WORK}/results.json", "w") as f:
        json.dump(results, f, ensure_ascii=False, indent=1)
    print("F3 PHASE DONE", json.dumps(results["phases"], ensure_ascii=False), flush=True)


if __name__ == "__main__":
    main()
