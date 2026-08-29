#!/usr/bin/env python3
"""修正後のCセッション (S3/S4/S5/S6/S9) のみ再実行し、results.jsonの該当キーを差し替える。"""
import json
import run_b

with open(f"{run_b.WP7}/results.json") as f:
    run_b.results.update(json.load(f))
for k in list(run_b.results):
    if k.startswith(("s3", "s4", "s5", "s6", "s9")):
        del run_b.results[k]

run_b.session_C_cold()
run_b.session_C_warm()
run_b.session_C_virtual()

with open(f"{run_b.WP7}/results.json", "w") as f:
    json.dump(run_b.results, f, ensure_ascii=False, indent=1)
print("PART2 DONE")
