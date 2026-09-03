import os, re, subprocess, time, json, signal
BIN = "/mnt/holyland/Project/GEN-TO/target/release/gento-tauri"
W = "/tmp/opencode/wp16/s7"
ENV = dict(os.environ, DISPLAY=":6", GENTO_LOG="info", GENTO_DEV_BRIDGE="1", NO_COLOR="1")
log_path = f"{W}/logs/app.log"; open(log_path, "wb").close()
env = dict(ENV, HOME=f"{W}/home", XDG_DATA_HOME=f"{W}/home/.local/share", XDG_CACHE_HOME=f"{W}/home/.cache")
log = open(log_path, "ab", buffering=0)
t0 = time.time()
proc = subprocess.Popen([BIN, "--dataset", "/mnt/holyland/Project/GEN-TO/sample/dataset",
    "--cache-dir", f"{W}/thumbs", "--db", f"{W}/home/library.sqlite3",
    "--models-dir", "/mnt/holyland/Project/GEN-TO/.local-models"],
    stdout=log, stderr=subprocess.STDOUT, env=env, cwd="/tmp/opencode/wp16")
r = ""
for _ in range(150):
    r = subprocess.run(["xdotool", "search", "--onlyvisible", "--pid", str(proc.pid)],
                       capture_output=True, text=True, env=ENV).stdout.strip()
    if r: break
    time.sleep(0.2)
time.sleep(1.5)
if r:
    subprocess.run(["xdotool", "windowactivate", "--sync", r.split("\n")[0]], capture_output=True, env=ENV)
def key(*a): subprocess.run(["xdotool", "key", *a], capture_output=True, env=ENV)
def snap():
    txt = open(log_path, errors="replace").read()
    for line in reversed(txt.splitlines()):
        if "devtest:" in line:
            d = {}
            for t in re.finditer(r"([A-Za-z0-9_]+)=([^\s|]+)", line.split("devtest:",1)[1]):
                k = t.group(1)
                if k in d: continue
                try: d[k] = float(t.group(2))
                except ValueError: d[k] = t.group(2)
            return d
    return {}
def wait_jobs(min_done=10, timeout=240):
    t = time.time()
    last = -1
    while time.time() - t < timeout:
        import sqlite3
        try:
            con = sqlite3.connect(f"file:{W}/home/library.sqlite3?mode=ro", uri=True)
            done = con.execute("SELECT COUNT(*) FROM jobs WHERE job_type='embedding' AND status IN ('succeeded','failed')").fetchone()[0]
            con.close()
        except Exception:
            done = 0
        if done >= min_done and done == last:
            return done, time.time() - t
        last = done
        time.sleep(2)
    return last, time.time() - t
# 処理開始待ち (まず10件完了で「処理中」を確認)
done0, w0 = wait_jobs(10, 240)
print("warm: 10 jobs in", round(w0,1), "s", flush=True)
res = {"warmup_s": round(w0,1)}
# 処理中に keyword検索
def search(q):
    key("ctrl+shift+F12"); time.sleep(0.4); key("h"); time.sleep(0.5)
    subprocess.run(["xdotool", "type", "--delay", "40", q], capture_output=True, env=ENV); time.sleep(0.3)
    key("Return"); time.sleep(4)
    s = snap()
    return {"query": q, "tot": s.get("tot"), "ms": s.get("ms")}
res["keyword_during_embed"] = search("00000")
# ハイブリッド: 検索input focus → Tab 2回 → Right×10 (50%) → 打鍵→Return
key("ctrl+shift+F12"); time.sleep(0.4); key("h"); time.sleep(0.6)
key("Tab"); time.sleep(0.2); key("Tab"); time.sleep(0.2)
for _ in range(10):
    key("Right"); time.sleep(0.12)
subprocess.run(["import", "-window", "root", f"{W}/logs/slider.png"], capture_output=True, env=ENV)
subprocess.run(["xdotool", "type", "--delay", "40", "00000"], capture_output=True, env=ENV); time.sleep(0.3)
key("Return"); time.sleep(5)
s = snap()
res["hybrid_during_embed"] = {"query": "00000", "tot": s.get("tot"), "ms": s.get("ms")}
# スクロール
key("Escape"); time.sleep(0.6); key("ctrl+F2"); time.sleep(0.8)
key("Home"); time.sleep(1.0)
b = snap()
key("--repeat", "40", "--delay", "40", "Page_Down")
time.sleep(4)
a = snap()
res["scroll_during_embed"] = {
    "long_frames": a.get("lf",0)-b.get("lf",0), "worst_frame_ms": a.get("lfMax"),
    "dom": a.get("dom"), "thumb_proto_p50": a.get("pp50"), "q_settled": a.get("q")}
# 全処理完了まで計測
t1 = time.time()
done, w1 = wait_jobs(974, 1800)
res["full_embed"] = {"done_jobs": done, "wall_s": round(time.time()-t0, 1), "note": "974 assets (1615 files dedup)"}
s = snap()
res["final"] = {"lf": s.get("lf"), "lfMax": s.get("lfMax")}
proc.send_signal(signal.SIGTERM)
try: proc.wait(timeout=60)
except Exception: proc.kill()
json.dump(res, open(f"{W}/results.json","w"), ensure_ascii=False, indent=1)
print(json.dumps(res, ensure_ascii=False, indent=1))
