#!/usr/bin/env python3
"""Click-through review of suspicious training crops.

  .venv/bin/python tools/review_server.py      then open http://localhost:8765

Shows the crops the audit flagged (art/audit_torch.csv), grouped by the value they
are labelled as and by capture scene. Click a picture that shows a DIFFERENT
number than its heading to mark it (red); "Save" writes the marked ones to
crop_overrides.tsv as `drop` lines (the previous file is kept as .bak). Nothing in
data/ is modified. Re-opening the page restores earlier marks from the file.
"""
from __future__ import annotations

import csv
import html
import json
import re
import shutil
from collections import defaultdict
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import unquote

ROOT = Path(__file__).resolve().parent.parent
AUDIT = ROOT / "art" / "audit_torch.csv"
CROPS = ROOT / "data" / "dice_face_crops"
OVERRIDES = ROOT / "crop_overrides.tsv"
MAX_PROB = 0.1  # only crops the model gave < 10% to their own label
PORT = 8765


def scene(stem: str) -> str:
    """'9d20_wood0712' -> 'd20_wood' so runs of one die group together."""
    s = re.sub(r"^\d+", "", stem)
    s = re.sub(r"_jpg\.rf\.[0-9a-f]+$", "", s)
    s = re.sub(r"^[0-9a-f]{4,}-", "", s)
    s = re.sub(r"(IMG_\d{8}_\d{4}).*", r"\1", s)
    return re.sub(r"\d+$", "", s) or s


def load_marked() -> set[str]:
    if not OVERRIDES.exists():
        return set()
    return {l.split("\t")[0].strip() for l in OVERRIDES.read_text().splitlines()
            if "\t" in l and l.split("\t")[1].split("#")[0].strip() == "drop"}


def page() -> str:
    rows = [r for r in csv.DictReader(open(AUDIT)) if float(r["label_prob"]) < MAX_PROB]
    by_label: dict[str, dict[str, list[dict]]] = defaultdict(lambda: defaultdict(list))
    for r in rows:
        folder, stem = r["key"].split("/", 1)
        by_label[r["label"]][scene(stem)].append(r)
    marked = load_marked()

    parts = []
    nav = []
    for label in sorted(by_label, key=int):
        groups = by_label[label]
        n = sum(len(g) for g in groups.values())
        nav.append(f'<a href="#l{label}">{label} <small>({n})</small></a>')
        parts.append(f'<section id="l{label}"><h2>Should show: <b>{label}</b> <small>{n} pictures</small></h2>')
        for sc, items in sorted(groups.items(), key=lambda kv: -len(kv[1])):
            parts.append(f'<div class="group"><div class="ghead">{html.escape(sc)} &middot; {len(items)} '
                         f'<button onclick="markGroup(this,true)">mark all</button>'
                         f'<button onclick="markGroup(this,false)">unmark all</button></div><div class="imgs">')
            for r in items:
                folder, stem = r["key"].split("/", 1)
                cls = "img marked" if r["key"] in marked else "img"
                parts.append(f'<img class="{cls}" data-key="{html.escape(r["key"])}" loading="lazy" '
                             f'src="/img/{folder}/{html.escape(stem)}.png" onclick="toggle(this)">')
            parts.append("</div></div>")
        parts.append("</section>")

    return """<!doctype html><html><head><meta charset="utf-8"><title>Crop review</title><style>
body{font-family:system-ui,sans-serif;margin:0;background:#f4f4f4;color:#222}
header{position:sticky;top:0;background:#222;color:#fff;padding:10px 16px;z-index:2}
header p{margin:4px 0;font-size:14px} nav a{color:#9cf;margin-right:10px;font-size:14px}
#save{float:right;font-size:16px;padding:8px 18px;background:#2a7;color:#fff;border:0;border-radius:4px;cursor:pointer}
#count{float:right;margin:10px 14px;font-size:14px}
section{padding:8px 16px} h2{margin:18px 0 6px} h2 b{font-size:28px;color:#c00}
.group{background:#fff;margin:8px 0;padding:6px;border-radius:4px}
.ghead{font-size:13px;color:#555;margin-bottom:4px} .ghead button{margin-left:6px;font-size:12px}
.img{width:96px;height:96px;margin:2px;border:4px solid transparent;cursor:pointer;image-rendering:auto}
.img.marked{border-color:#e00;opacity:.55}
</style></head><body><header>
<button id="save" onclick="save()">Save</button><span id="count"></span>
<p><b>Each picture should show the red number in its heading.</b> Upside down, sideways or blurry is fine,
so leave those alone. <b>Click a picture that shows a DIFFERENT number</b> and it turns red. "mark all" marks a whole group.
Press <b>Save</b> when done. You can close the page and come back; saved marks are kept.</p>
<nav>""" + "".join(nav) + "</nav></header>" + "".join(parts) + """
<script>
function update(){document.getElementById('count').textContent=
  document.querySelectorAll('.img.marked').length+' marked'}
function toggle(el){el.classList.toggle('marked');update()}
function markGroup(btn,on){btn.closest('.group').querySelectorAll('.img').forEach(
  i=>i.classList.toggle('marked',on));update()}
async function save(){
  const keys=[...document.querySelectorAll('.img.marked')].map(i=>i.dataset.key);
  const r=await fetch('/save',{method:'POST',body:JSON.stringify(keys)});
  alert(await r.text())}
update()
</script></body></html>"""


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/":
            body = page().encode()
            self._send(200, "text/html; charset=utf-8", body)
        elif self.path.startswith("/img/"):
            p = (CROPS / unquote(self.path[5:])).resolve()
            if CROPS.resolve() in p.parents and p.is_file():
                self._send(200, "image/png", p.read_bytes())
            else:
                self._send(404, "text/plain", b"not found")
        else:
            self._send(404, "text/plain", b"not found")

    def do_POST(self):
        if self.path != "/save":
            return self._send(404, "text/plain", b"not found")
        keys = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        # Keep any lines this page does not manage (relabels, comments).
        kept = []
        if OVERRIDES.exists():
            shutil.copy2(OVERRIDES, OVERRIDES.with_suffix(".tsv.bak"))
            kept = [l for l in OVERRIDES.read_text().splitlines()
                    if not ("\t" in l and l.split("\t")[1].split("#")[0].strip() == "drop")]
        lines = kept + [f"{k}\tdrop" for k in sorted(set(keys))]
        OVERRIDES.write_text("\n".join(lines) + "\n")
        self._send(200, "text/plain", f"Saved {len(set(keys))} marked pictures to crop_overrides.tsv".encode())

    def _send(self, code, ctype, body):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *a):
        pass


if __name__ == "__main__":
    print(f"Review page: http://localhost:{PORT}   (Ctrl+C to stop)")
    ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
