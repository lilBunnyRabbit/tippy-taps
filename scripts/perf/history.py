import json
import random
import sys
from datetime import date, timedelta
from pathlib import Path

out = Path(sys.argv[1]) / "tippy-taps" / "counts"
layout = sys.argv[2]
days = int(sys.argv[3]) if len(sys.argv) > 3 else 365
rng = random.Random(13)
fields = ["keys", "clicks", "scrolls", "m_left", "m_right", "m_middle", "m_px", "m_um",
          "m_dir_n", "m_dir_ne", "m_dir_e", "m_dir_se", "m_dir_s", "m_dir_sw", "m_dir_w", "m_dir_nw"]

files = {}
today = date.today()
for back in range(days):
    d = today - timedelta(days=back)
    for h in range(8, 21):
        bucket = {f: rng.randint(1, 5000) for f in fields}
        name = f"{d:%Y-%m}.json" if layout == "months" else f"{d:%Y}/{d:%m-%d}.json"
        files.setdefault(name, {})[f"{d:%Y-%m-%d}T{h:02}"] = bucket

for name, hours in files.items():
    path = out / name
    path.parent.mkdir(parents=True, exist_ok=True)
    body = {"version": 2, "hours": hours}
    text = json.dumps(body, indent=2) if layout == "months" else json.dumps(body, separators=(",", ":"))
    path.write_text(text + "\n", encoding="utf-8")
print(f"{len(files)} files in {out}")
