#!/usr/bin/env python3
"""Download shared and flagged roll pictures from SurrealDB for training.

Pictures land in data_incoming/ (never data/), one folder per day:
    data_incoming/2026-09-25/<user>__<roll_id>.jpg
    data_incoming/2026-09-25/<user>__<roll_id>.json   (roll, detections, flags,
                                                       the user's corrections, reason)

Only reads from the database; progress is kept in data_incoming/.pull_state.json.
Samples that change later (e.g. flagged after being sampled) are downloaded again
and overwrite the old files. When a user deletes what they shared, the site logs
it and the next pull removes their earlier files here too.

Login comes from the environment, never from arguments (they end up in shell
history): SURREAL_USER / SURREAL_PASS, a read-only (VIEWER) user on the arcane
database; see services/frontend/surreal/README.md.

    SURREAL_USER=dice_reader SURREAL_PASS=... \\
        .venv/bin/python tools/pull_dataset.py --url https://<surreal>/  [--dry-run]
"""
from __future__ import annotations

import argparse
import base64
import json
import os
import re
import sys
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PAGE = 25

SAMPLES = """
SELECT username, roll_id, auto_reason, flagged, flagged_at, user_values, roll, frame,
       model, review_status, created_at, updated_at,
       encoding::base64::encode(image.jpeg) AS jpeg
FROM roll_sample WHERE updated_at >= <datetime> $since
ORDER BY updated_at ASC LIMIT $page
"""
DELETIONS = "SELECT username, at FROM dataset_deletion WHERE at > <datetime> $since ORDER BY at ASC"


class Surreal:
    def __init__(self, url: str, ns: str, db: str, user: str, password: str):
        self.url = url.rstrip("/") + "/rpc"
        auth = base64.b64encode(f"{user}:{password}".encode()).decode()
        self.headers = {
            "Content-Type": "application/json",
            "Accept": "application/json",
            "Authorization": f"Basic {auth}",
            "surreal-ns": ns,
            "surreal-db": db,
            "surreal-auth-ns": ns,
            "surreal-auth-db": db,
        }

    def query(self, sql: str, **vars) -> list:
        body = json.dumps({"id": 1, "method": "query", "params": [sql, vars]}).encode()
        req = urllib.request.Request(self.url, data=body, headers=self.headers)
        try:
            with urllib.request.urlopen(req, timeout=60) as r:
                out = json.loads(r.read())
        except urllib.error.HTTPError as e:
            sys.exit(f"SurrealDB refused the request ({e.code}): {e.read().decode()[:300]}")
        if "error" in out:
            sys.exit(f"SurrealDB error: {out['error']}")
        results = []
        for stmt in out["result"]:
            if stmt["status"] != "OK":
                sys.exit(f"SurrealDB error: {stmt['result']}")
            results.append(stmt["result"])
        return results[-1]


def when(stamp: str) -> datetime:
    """SurrealDB datetimes ("...T08:36:52.8993Z", nanoseconds, trailing zeros
    trimmed) as comparable values; text comparison would get them wrong."""
    m = re.fullmatch(r"(.*T\d\d:\d\d:\d\d)(?:\.(\d+))?Z", stamp)
    if not m:
        raise ValueError(f"unexpected datetime {stamp!r}")
    frac = (m.group(2) or "").ljust(6, "0")[:6]
    return datetime.fromisoformat(f"{m.group(1)}.{frac}+00:00")


def check_url(url: str) -> None:
    """Basic auth over plain http is only OK to this PC (e.g. a kubectl port-forward)."""
    u = urllib.parse.urlparse(url)
    if u.scheme == "https" or (u.scheme == "http" and u.hostname in ("localhost", "127.0.0.1", "::1")):
        return
    sys.exit("Use an https:// URL (plain http is only allowed to localhost, e.g. a port-forward).")


def safe(name: str) -> str:
    """File-name-safe version of a username or roll id."""
    return "".join(c if c.isalnum() or c in "-_." else "_" for c in name)


def load_state(state_file: Path) -> dict:
    if state_file.exists():
        return json.loads(state_file.read_text())
    epoch = "1970-01-01T00:00:00Z"
    return {"samples_since": epoch, "seen_at_since": [], "deletions_since": epoch}


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--url", required=True, help="SurrealDB base URL (reachable from this PC)")
    ap.add_argument("--ns", default="milesstorm")
    ap.add_argument("--db", default="arcane")
    ap.add_argument("--dry-run", action="store_true", help="only list what would change")
    ap.add_argument("--out", type=Path, default=ROOT / "data_incoming", help="download folder (never data/)")
    args = ap.parse_args()
    OUT = args.out
    if OUT.resolve() == (ROOT / "data").resolve() or (ROOT / "data").resolve() in OUT.resolve().parents:
        sys.exit("Refusing to write into data/: pick another --out.")
    STATE = OUT / ".pull_state.json"

    check_url(args.url)
    user, password = os.environ.get("SURREAL_USER"), os.environ.get("SURREAL_PASS")
    if not user or not password:
        sys.exit("Set SURREAL_USER and SURREAL_PASS (a read-only user) in the environment.")
    db = Surreal(args.url, args.ns, args.db, user, password)
    state = load_state(STATE)
    state.setdefault("seen_at_since", [])

    def save_state() -> None:
        if not args.dry_run:
            OUT.mkdir(parents=True, exist_ok=True)
            STATE.write_text(json.dumps(state, indent=1))

    new = updated = removed = 0

    # 1. Remove local copies for users who deleted what they shared. Files on disk
    #    are scanned (not a list), so nothing is missed after an interrupted run.
    deletions = db.query(DELETIONS, since=state["deletions_since"])
    for d in deletions:
        cutoff = when(d["at"])
        for meta in sorted(OUT.glob(f"*/{safe(d['username'])}__*.json")):
            info = json.loads(meta.read_text())
            if info.get("username") != d["username"] or when(info["created_at"]) > cutoff:
                continue
            print(f"remove {meta.parent.name}/{meta.stem} (user deleted their pictures)")
            if not args.dry_run:
                meta.with_suffix(".jpg").unlink(missing_ok=True)
                meta.unlink()
            removed += 1
    if deletions:
        state["deletions_since"] = deletions[-1]["at"]
    save_state()

    # 2. Download new and changed samples, oldest change first. Paging uses
    #    updated_at >= cursor and skips what was already taken at exactly the
    #    cursor, so samples sharing a timestamp are never skipped.
    while True:
        page = db.query(SAMPLES, since=state["samples_since"], page=PAGE)
        fresh = [s for s in page if not (s["updated_at"] == state["samples_since"]
                                         and f"{s['username']}/{s['roll_id']}" in state["seen_at_since"])]
        for s in fresh:
            key = f"{safe(s['username'])}__{safe(s['roll_id'])}"
            day = s["created_at"][:10]
            folder = OUT / day
            is_new = not (folder / f"{key}.json").exists()
            tags = ["flagged" if s["flagged"] else "", s.get("auto_reason") or ""]
            print(f"{'new ' if is_new else 'update'} {day}/{key} {' '.join(t for t in tags if t)}")
            if not args.dry_run:
                folder.mkdir(parents=True, exist_ok=True)
                jpeg = s.pop("jpeg") or ""
                (folder / f"{key}.jpg").write_bytes(base64.b64decode(jpeg + "=" * (-len(jpeg) % 4)))
                (folder / f"{key}.json").write_text(json.dumps(s, indent=1))
            new += is_new
            updated += not is_new
            if s["updated_at"] != state["samples_since"]:
                state["samples_since"], state["seen_at_since"] = s["updated_at"], []
            state["seen_at_since"].append(f"{s['username']}/{s['roll_id']}")
        save_state()
        if len(page) < PAGE or not fresh:
            break

    print(f"\n{new} new, {updated} updated, {removed} removed{' (dry run: nothing written)' if args.dry_run else ''}")


if __name__ == "__main__":
    main()
