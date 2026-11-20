import csv
import sys
from collections import defaultdict
from statistics import mean

rows = list(csv.DictReader(open(sys.argv[1], encoding="utf-8-sig")))
metrics = ["cpu_pct", "ws_mb", "private_mb", "handles", "threads", "io_read_bytes", "io_write_bytes"]
groups = defaultdict(list)
for r in rows:
    groups[(r["scenario"], r["build"])].append(r)
order = ["hidden-idle", "hidden-moving", "open-idle", "open-moving"]
print("| scenario | build | runs | " + " | ".join(f"{m} mean / max" for m in metrics) + " |")
print("|" + "---|" * (3 + len(metrics)))
for scenario in order:
    for build in ["base", "head"]:
        g = groups.get((scenario, build), [])
        if not g:
            continue
        runs = len({r["run"] for r in g})
        cells = []
        for m in metrics:
            v = [float(r[m]) for r in g]
            cells.append(f"{mean(v):.3f} / {max(v):.3f}" if m == "cpu_pct" else f"{mean(v):.1f} / {max(v):.0f}")
        print(f"| {scenario} | {build} | {runs} | " + " | ".join(cells) + " |")
print()
print("CPU % mean per run:")
for scenario in order:
    for build in ["base", "head"]:
        per = defaultdict(list)
        for r in groups.get((scenario, build), []):
            per[r["run"]].append(float(r["cpu_pct"]))
        if per:
            print(f"  {scenario} {build}: " + ", ".join(f"{mean(v):.3f}" for _, v in sorted(per.items())))
