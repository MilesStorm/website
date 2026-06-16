#!/usr/bin/env python3
"""Send a held-out dice image to the running inference WebSocket and print the reply.

Smoke-tests the full Rust pipeline (YOLO bbox -> crop -> DiceHead) on a real image
without needing physical dice. Start the server first:

    cargo run --release -- yolo 0.0.0.0:9000

Then:

    python tools/ws_probe.py [image.jpg] [ws://127.0.0.1:9000]

The server expects a binary frame (JPEG/PNG bytes) and replies with JSON:
  {"detections":[{x1,y1,x2,y2,yolo_conf,yolo_class,dice_class,dice_conf},...],"frame_ms":N}
"""
from __future__ import annotations

import asyncio
import json
import sys
from pathlib import Path

import websockets

ROOT = Path(__file__).resolve().parent.parent
VAL_IMG = ROOT / "data" / "yolo_det" / "images" / "val"


async def probe(img_path: Path, url: str) -> None:
    data = img_path.read_bytes()
    print(f"sending {img_path.name} ({len(data)} bytes) to {url}")
    async with websockets.connect(url, max_size=None) as ws:
        await ws.send(data)
        # First frame is slow: CubeCL autotunes/compiles kernels on cold start.
        reply = await asyncio.wait_for(ws.recv(), timeout=240)
    parsed = json.loads(reply)
    print(json.dumps(parsed, indent=2))
    dets = parsed.get("detections", [])
    print(f"\n{len(dets)} detection(s); frame_ms={parsed.get('frame_ms')}")
    assert dets, "no detections returned"


def main() -> None:
    img = Path(sys.argv[1]) if len(sys.argv) > 1 else sorted(VAL_IMG.glob("*.jpg"))[0]
    url = sys.argv[2] if len(sys.argv) > 2 else "ws://127.0.0.1:9000"
    asyncio.run(probe(img, url))


if __name__ == "__main__":
    main()
