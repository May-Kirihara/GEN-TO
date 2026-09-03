import os, re, subprocess, time, json
BIN = "/mnt/holyland/Project/GEN-TO/target/release/gento-tauri"
W = "/tmp/opencode/wp16/s6"
ENV = dict(os.environ, DISPLAY=":6", GENTO_LOG="info", GENTO_DEV_BRIDGE="1", NO_COLOR="1")
log_path = f"{W}/logs/app-jobs.log"; open(log_path, "wb").close()
env = dict(ENV, HOME=f"{W}/home", XDG_DATA_HOME=f"{W}/home/.local/share", XDG_CACHE_HOME=f"{W}/home/.cache")
log = open(log_path, "ab", buffering=0)
proc = subprocess.Popen([BIN, "--dataset", "/mnt/holyland/Project/GEN-TO/sample/dataset",
    "--cache-dir", f"{W}/thumbs", "--db", f"{W}/home/library.sqlite3"],
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
res = {}
t0 = time.time()
# ジョブ開始待ち (runningになる = mockの遅延窓に入る)
time.sleep(6)
s = snap()
print("snap after 6s:", {k: s.get(k) for k in ("tot","lf","lfMax","q","hr")}, flush=True)
# その間にUI操作: 検索・スクロール・比較viewer
def search(q):
    b = snap()
    key("ctrl+shift+F12"); time.sleep(0.4); key("h"); time.sleep(0.5)
    subprocess.run(["xdotool", "type", "--delay", "40", q], capture_output=True, env=ENV); time.sleep(0.3)
    key("Return")
    t = time.time()
    while time.time() - t < 30:
        s = snap()
        if s.get("q") == q and s.get("run") == 0 and s.get("act") == 1:
            break
        time.sleep(0.25)
    s = snap()
    return {"query": q, "wall_s": round(time.time()-t, 2), "tot": s.get("tot"), "ms": s.get("ms")}
res["search_during_jobs"] = search("00000")
# スクロール
key("Escape"); time.sleep(0.8); key("ctrl+F2"); time.sleep(1.0)
key("Home"); time.sleep(1.2)
b = snap(); t0lf, t0q = b.get("lf",0), time.time()
key("--repeat", "40", "--delay", "40", "Page_Down")
time.sleep(4)
a = snap()
res["scroll_during_jobs"] = {
    "keys_wall_s": round(time.time()-t0q-4+1.6, 2),
    "long_frames": a.get("lf",0)-b.get("lf",0), "worst_frame_ms": a.get("lfMax"),
    "dom": a.get("dom"), "q_settled": a.get("q"), "thumb_p50": a.get("p50"), "proto_p50": a.get("pp50")}
# 比較viewer (2枚)
key("Home"); time.sleep(1.0)
b = snap()
key("F7"); time.sleep(0.5)   # select2 → 2枚比較?
key("ctrl+F3"); time.sleep(0.4)
key("F3"); time.sleep(4)     # viewer open (compare)
a = snap()
res["compare_during_jobs"] = {
    "long_frames": a.get("lf",0)-b.get("lf",0), "worst_frame_ms": a.get("lfMax"),
    "img_ok": a.get("ok"), "note": "viewer opened while AI jobs pending"}
key("Escape"); time.sleep(0.5)
# ジョブ進捗とworker状態をログから
time.sleep(10)
txt = open(log_path, errors="replace").read()
res["job_lines"] = [l.split("INFO ")[-1][:100] for l in txt.splitlines() if "ai" in l.lower() and "devtest" not in l][:8]
proc.send_signal(30); proc.wait(timeout=30) if False else None
import signal as sg
proc.send_signal(sg.SIGTERM)
try: proc.wait(timeout=30)
except Exception: proc.kill()
json.dump(res, open(f"{W}/results.json","w"), ensure_ascii=False, indent=1)
print(json.dumps(res, ensure_ascii=False, indent=1))
