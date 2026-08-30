#!/usr/bin/env python3
"""MVP WP-4 UIシェル性能計測 (S3相当: 高速スクロール / S4相当: 連続切替)。

スパイク計測 (docs/dev/measurements/run_b.py, S3/S4) の3ペイン製品UI版。
リリース (devtools) ビルド + GENTO_DEV_BRIDGE=1 で起動し、dev bridgeの
F-keyアクションと devtest: ログ行から数値を採取する。

実行:
  python3 docs/dev/measurements/drivers/wp4_ui_perf.py [--runs N]

新UI固有の決定論化:
  - WebView localStorage (レイアウト/フォルダ/セッション) は XDG_DATA_HOME を
    分離 + ctrl+F1 (dev bridge resetStorage) で初期化
  - 起動時の自動スキャン (--dataset) を利用 (従来のF8スキャン相当)

比較基準 (スパイク実績、docs/dev/measurements/results.json):
  S3: long_frames増分 0〜1 / worst_frame ~109ms / 要求・キャンセルが機能
  S4: nav p50 ~27ms (p95 45-56ms) / long_frames 0〜2
"""
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time

BIN = "/mnt/holyland/Project/GEN-TO/target/release/gento-tauri"
DATASET = "/mnt/holyland/Project/GEN-TO/sample/dataset"
WORK = "/tmp/opencode/wp4"
RESULTS = "/mnt/holyland/Project/GEN-TO/docs/dev/measurements/results.json"
TOTAL = 1615

ANSI = re.compile(r"\x1b\[[0-9;]*m")
DEVTEST = re.compile(r"devtest:\s*(.*)")


def base_env(xdg_root: str) -> dict:
    return dict(
        os.environ,
        DISPLAY=":1",
        GENTO_LOG="info",
        GENTO_DEV_BRIDGE="1",
        NO_COLOR="1",
        XDG_DATA_HOME=os.path.join(xdg_root, "data"),
        XDG_CACHE_HOME=os.path.join(xdg_root, "cache"),
    )


class App:
    # (既存のlaunch/key/snap等は変更なし)

    def reset_storage_and_wait(self, timeout: float = 40) -> None:
        """ctrl+F1 (localStorage初期化+再読み込み) を送り、再読み込み後の
        カタログを待つ (計測の決定論化に使用)。再読み込み中はdevtest行が
        止まるため、決定的なsleep後にtotを待つ。"""
        self.key("ctrl+F1")
        time.sleep(3.0)
        self.wait_for(lambda s: s["tot"] == TOTAL, timeout, "catalog after reload")
    def __init__(self, log_path: str, cache_dir: str, xdg_root: str):
        self.log_path = log_path
        os.makedirs(xdg_root, exist_ok=True)
        self.log = open(log_path, "ab", buffering=0)
        env = base_env(xdg_root)
        self.proc = subprocess.Popen(
            [BIN, "--dataset", DATASET, "--cache-dir", cache_dir],
            stdout=self.log,
            stderr=subprocess.STDOUT,
            env=env,
            cwd=WORK,
        )
        self.xdg_root = xdg_root
        wid = None
        for _ in range(120):
            r = subprocess.run(
                ["xdotool", "search", "--onlyvisible", "--pid", str(self.proc.pid)],
                capture_output=True, text=True,
            )
            if r.stdout.strip():
                wid = r.stdout.strip().split("\n")[0]
                break
            if self.proc.poll() is not None:
                raise RuntimeError(f"app exited early: see {log_path}")
            time.sleep(0.25)
        if wid is None:
            raise RuntimeError("app window not found")
        subprocess.run(["xdotool", "windowactivate", "--sync", wid], capture_output=True)
        time.sleep(1.5)

    def key(self, *args: str) -> None:
        subprocess.run(["xdotool", "key", *args], capture_output=True)

    def snap(self) -> dict:
        text = open(self.log_path, "rb").read().decode("utf-8", errors="replace")
        for line in reversed(text.splitlines()):
            clean = ANSI.sub("", line)
            m = DEVTEST.search(clean)
            if not m:
                continue
            snap: dict = {}
            for token in re.finditer(r"([A-Za-z0-9_]+)=([^\s|]+)", m.group(1)):
                k, v = token.group(1), token.group(2)
                try:
                    snap[k] = float(v)
                except ValueError:
                    snap[k] = v
            return snap
        return {}

    def wait_for(self, pred, timeout: float, what: str) -> dict:
        deadline = time.monotonic() + timeout
        last = {}
        while time.monotonic() < deadline:
            last = self.snap()
            try:
                if pred(last):
                    return last
            except KeyError:
                pass
            time.sleep(0.25)
        raise TimeoutError(f"waiting for {what}, last={last}")

    def terminate(self) -> None:
        self.proc.terminate()
        try:
            self.proc.wait(timeout=20)
        except subprocess.TimeoutExpired:
            self.proc.kill()


def record(key: str, run: int, values: dict) -> None:
    data = {}
    if os.path.exists(RESULTS):
        with open(RESULTS) as f:
            try:
                data = json.load(f)
            except json.JSONDecodeError:
                data = {}
    entries = data.setdefault(key, [])
    entries.append({"run": run, **values})
    with open(RESULTS, "w") as f:
        json.dump(data, f, indent=2, ensure_ascii=False)
    print(f"[record] {key} run={run}: {values}")


def scenario_s3(runs: int) -> None:
    """S3相当: コールドキャッシュで生成しながら高速スクロール。"""
    for run in range(1, runs + 1):
        root = os.path.join(WORK, f"s3-r{run}")
        cache = os.path.join(root, "thumbs")
        shutil.rmtree(root, ignore_errors=True)
        app = App(os.path.join(WORK, f"logs/wp4-s3-r{run}.log"), cache, root)
        try:
            # UI状態リセット (前回実行のlocalStorage掃除) → 再読み込み後の
            # カタログ復帰を待つ (起動時の自動スキャン)
            app.reset_storage_and_wait()
            app.key("ctrl+F2")  # グリッドフォーカス
            time.sleep(1.0)
            app.key("Home")
            time.sleep(1.2)
            before = app.snap()
            t0 = time.monotonic()
            app.key("--repeat", "40", "--delay", "40", "Page_Down")
            keys_wall = time.monotonic() - t0
            time.sleep(3.0)  # 収束待ち
            after = app.snap()

            def d(k: str) -> float:
                return after.get(k, 0.0) - before.get(k, 0.0)

            record("wp4_s3_fast_scroll", run, {
                "keys_wall_s": round(keys_wall, 2),
                "long_tasks": d("lt"),
                "long_frames": d("lf"),
                "worst_frame_ms": after.get("lfMax"),
                "requests": d("req"),
                "ready_recv": d("rdy"),
                "generated": d("gen"),
                "cancelled": d("can"),
                "q_after": after.get("q"),
                "dom": after.get("dom"),
            })
        finally:
            app.terminate()


def wait_generation_settled(app: "App", timeout: float) -> None:
    """生成キューが動き出して (q>0) 空になる (q==0) まで待つ。
    すでに全件キャッシュ済みでqが動かない場合はタイムアウトで諦める
    (ヒットで済んでいるため生成は不要)。"""
    started = False
    deadline = time.monotonic() + timeout
    last_change = time.monotonic()
    last_gen = -1.0
    while time.monotonic() < deadline:
        s = app.snap()
        gen = s.get("gen", 0.0)
        q = s.get("q", 0.0)
        if q > 0:
            started = True
        if gen != last_gen:
            last_gen = gen
            last_change = time.monotonic()
        if started and q == 0 and time.monotonic() - last_change > 2.0:
            return
        if not started and time.monotonic() - last_change > 30.0:
            print("[warm] queue never moved; assuming fully cached")
            return
        time.sleep(1.0)
    raise TimeoutError("generation did not settle")


def warm_cache_for_s4(warm_root: str, warm_cache: str) -> None:
    """S4用に s256 + preview2048 を全件生成する (historicalなs456 warmup相当)。"""
    log = os.path.join(WORK, "logs/wp4-s4-warmup.log")
    open(log, "wb").close()
    app = App(log, warm_cache, warm_root)
    try:
        app.wait_for(lambda s: s["tot"] == TOTAL, 90, "catalog total")
        app.key("ctrl+F8")  # 全件生成 (s256)
        wait_generation_settled(app, 600)
        print("[warm] s256 settled")
        app.key("shift+F8")  # preview2048 全件生成
        wait_generation_settled(app, 1800)
        print("[warm] preview2048 settled")
    finally:
        app.terminate()


def scenario_s4(runs: int) -> None:
    """S4相当: ウォームキャッシュ (s256+preview2048) で単一ビューア100枚移動。"""
    warm_root = os.path.join(WORK, "s4-warm")
    warm_cache = os.path.join(warm_root, "thumbs")
    os.makedirs(warm_root, exist_ok=True)
    warm_cache_for_s4(warm_root, warm_cache)
    for run in range(1, runs + 1):
        log = os.path.join(WORK, f"logs/wp4-s4-r{run}.log")
        open(log, "wb").close()
        app = App(log, warm_cache, warm_root)
        try:
            app.reset_storage_and_wait()
            app.key("ctrl+F2")
            time.sleep(0.5)
            app.key("Home")
            time.sleep(0.8)
            app.key("F2")   # 2件選択
            time.sleep(0.3)
            app.key("F3")   # ビューアで開く (「開く」ボタン)
            time.sleep(2.5)
            app.key("ctrl+F6")  # 統計リセット
            time.sleep(0.5)
            before = app.snap()
            t0 = time.monotonic()
            app.key("--repeat", "100", "--delay", "25", "Right")
            keys_wall = time.monotonic() - t0
            time.sleep(2.5)
            after = app.snap()

            def d(k: str) -> float:
                return after.get(k, 0.0) - before.get(k, 0.0)

            record("wp4_s4_viewer_nav", run, {
                "keys_wall_s": round(keys_wall, 2),
                "applied": after.get("ap"),
                "navN": after.get("onN"),
                "nav_p50_ms": after.get("onP50"),
                "nav_p95_ms": after.get("onP95"),
                "nav_p99_ms": after.get("onP99"),
                "stale": after.get("stale"),
                "long_frames": d("lf"),
                "worst_frame_ms": after.get("lfMax"),
            })
            app.key("Escape")
            time.sleep(0.8)
        finally:
            app.terminate()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--runs", type=int, default=3)
    args = parser.parse_args()
    os.makedirs(os.path.join(WORK, "logs"), exist_ok=True)
    if not os.path.exists(BIN):
        sys.exit(f"binary not found: {BIN} (build with devtools first)")
    scenario_s3(args.runs)
    scenario_s4(args.runs)
    print("done. results appended to", RESULTS)


if __name__ == "__main__":
    main()
