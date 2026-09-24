#!/usr/bin/env python3
"""Second review pass: give the pictures marked `drop` their correct number.

  .venv/bin/python tools/relabel_server.py      then open http://localhost:8766

Every `drop` line in crop_overrides.tsv is shown with the number the model reads
in it pre-selected (average of the given model runs, none of which trained on
these pictures). Confirm, or pick another number, or choose "unreadable".
Save rewrites those lines: a number -> relabel (`key<TAB>3`), "unreadable" ->
stays `drop`, the original folder number -> line removed (label was right).
The previous file is kept as crop_overrides.tsv.bak. data/ is never modified.
"""
from __future__ import annotations

import argparse
import html
import json
import shutil
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import unquote

import numpy as np
import torch
from PIL import Image

sys.path.insert(0, str(Path(__file__).resolve().parent))
import train_head_torch as T  # noqa: E402
from review_server import scene  # noqa: E402

OVERRIDES = T.OVERRIDES
PORT = 8766
NAMES = T.label_names()
CHOICES = [str(v) for v in range(0, 21)]  # folder-name values; "0" is the d10 zero


def read_overrides() -> list[tuple[str, str]]:
    out = []
    for line in OVERRIDES.read_text().splitlines():
        if "\t" in line:
            k, a = line.split("\t", 1)
            out.append((k.strip(), a.split("#")[0].strip()))
    return out


def guesses(keys: list[str], runs: list[Path]) -> dict[str, tuple[str, float]]:
    """key -> (value name, probability), averaged over the fold models."""
    imgs = []
    for k in keys:
        d, stem = k.split("/", 1)
        im = Image.open(T.CROPS / d / f"{stem}.png").convert("RGB").resize((T.SIZE, T.SIZE))
        imgs.append(np.asarray(im).transpose(2, 0, 1))
    x = torch.from_numpy(np.stack(imgs)).float().cuda() / 255
    probs = 0
    for run in runs:
        meta = json.loads((run / "metrics.json").read_text())
        m = T.build_model(meta["model"], meta.get("upscale", T.SIZE))
        m.load_state_dict(torch.load(run / "best.pt", map_location="cuda"))
        with torch.no_grad():
            probs = probs + torch.softmax(m.cuda().eval()(x).float(), 1)
    probs = (probs / len(runs)).cpu()
    return {k: (NAMES[int(p.argmax())], float(p.max())) for k, p in zip(keys, probs)}


def page(rows: list[tuple[str, str, float]]) -> str:
    """rows: (key, preselected value, model probability)."""
    cards, last = [], None
    for key, pre, prob in rows:
        folder, stem = key.split("/", 1)
        grp = f"{folder}/{scene(stem)}"
        if grp != last:
            cards.append(f'<h3>Folder said <b>{folder}</b> &middot; {html.escape(scene(stem))}</h3>')
            last = grp
        opts = "".join(f'<option value="{v}"{" selected" if v == pre else ""}>{v}</option>' for v in CHOICES)
        opts += f'<option value="unreadable"{" selected" if pre == "unreadable" else ""}>unreadable</option>'
        unsure = " unsure" if prob < 0.6 else ""
        cards.append(
            f'<div class="card{unsure}"><img loading="lazy" src="/img/{folder}/{html.escape(stem)}.png">'
            f'<select data-key="{html.escape(key)}" data-folder="{folder}">{opts}</select></div>')
    return """<!doctype html><html><head><meta charset="utf-8"><title>Relabel</title><style>
body{font-family:system-ui,sans-serif;margin:0;background:#f4f4f4;color:#222}
header{position:sticky;top:0;background:#222;color:#fff;padding:10px 16px;z-index:2}
header p{margin:4px 0;font-size:14px}
#save{float:right;font-size:16px;padding:8px 18px;background:#2a7;color:#fff;border:0;border-radius:4px;cursor:pointer}
main{padding:8px 16px} h3{margin:16px 0 4px;font-weight:normal} h3 b{color:#c00;font-size:20px}
.card{display:inline-block;margin:3px;padding:4px;background:#fff;border:3px solid #fff;border-radius:4px;text-align:center}
.card.unsure{border-color:#f0b400} .card img{width:128px;height:128px;display:block}
.card select{margin-top:4px;font-size:18px;width:128px}
</style></head><body><header><button id="save" onclick="save()">Save</button>
<p><b>Each picture was marked as NOT showing its folder's number.</b> The box under it is pre-filled with the number
the model reads in the picture.</p>
<p>If the box is right, do nothing. If not, pick the number the picture really shows. If you can't read it, pick
<b>unreadable</b>. <span style="color:#f0b400">Yellow border</span> = the model is unsure, so please look closely. Press <b>Save</b> when done.</p>
</header><main>""" + "".join(cards) + """</main><script>
async function save(){
  const c=[...document.querySelectorAll('select')].map(s=>[s.dataset.key,s.value]);
  const r=await fetch('/save',{method:'POST',body:JSON.stringify(c)});alert(await r.text())}
</script></body></html>"""


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("runs", nargs="*", default=[
        str(T.OUT / "resnet18_experiment_40_clean"), str(T.OUT / "resnet18_experiment_41_clean")])
    args = ap.parse_args()

    ov = read_overrides()
    marked = [k for k, a in ov if a == "drop"]
    relabelled = {k: a for k, a in ov if a != "drop"}
    g = guesses(marked, [Path(r) for r in args.runs]) if marked else {}
    # Revisit earlier relabels too, preselected with the saved choice.
    rows = [(k, g[k][0], g[k][1]) for k in marked] + [(k, a, 1.0) for k, a in relabelled.items()]
    rows.sort(key=lambda r: (int(r[0].split("/")[0]), scene(r[0].split("/", 1)[1]), r[0]))
    body = page(rows).encode()
    print(f"{len(rows)} pictures to confirm ({sum(p < 0.6 for _, _, p in rows)} marked unsure)")

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            if self.path == "/":
                return self._send(200, "text/html; charset=utf-8", body)
            p = (T.CROPS / unquote(self.path[5:])).resolve() if self.path.startswith("/img/") else None
            if p and T.CROPS.resolve() in p.parents and p.is_file():
                return self._send(200, "image/png", p.read_bytes())
            self._send(404, "text/plain", b"not found")

        def do_POST(self):
            choices = dict(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
            shutil.copy2(OVERRIDES, OVERRIDES.with_suffix(".tsv.bak"))
            lines, n = [], {"relabel": 0, "drop": 0, "restored": 0}
            for line in OVERRIDES.read_text().splitlines():
                key = line.split("\t", 1)[0].strip() if "\t" in line else None
                if key not in choices:
                    lines.append(line)  # comments / lines this page doesn't manage
                    continue
                c = choices[key]
                if c == "unreadable":
                    lines.append(f"{key}\tdrop"); n["drop"] += 1
                elif c == key.split("/")[0]:
                    n["restored"] += 1  # folder label was right after all
                else:
                    lines.append(f"{key}\t{c}"); n["relabel"] += 1
            OVERRIDES.write_text("\n".join(lines) + "\n")
            self._send(200, "text/plain", (f"Saved: {n['relabel']} relabelled, {n['drop']} unreadable (dropped), "
                                           f"{n['restored']} back to their folder number").encode())

        def _send(self, code, ctype, data):
            self.send_response(code)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def log_message(self, *a):
            pass

    print(f"Relabel page: http://localhost:{PORT}   (Ctrl+C to stop)")
    ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()


if __name__ == "__main__":
    main()
