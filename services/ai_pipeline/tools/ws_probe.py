#!/usr/bin/env python3
"""Send a held-out dice image to the running inference WebSocket and print the reply.

Smoke-tests the full Rust pipeline (YOLO bbox -> crop -> DiceHead) on a real image
without needing physical dice. Start the server first:

    cargo run --release -- yolo 0.0.0.0:9000

Then:

    python tools/ws_probe.py [image.jpg] [ws://127.0.0.1:9000] [repeat]

The server expects a binary frame (JPEG/PNG bytes) and replies with JSON:
  {"type":"frame","detections":[{...,"value","confident"}],"frame_ms":N}
and, once dice have settled, {"type":"roll",...}. With `repeat` > 1 the same image
is sent repeatedly (a die resting in view), waiting for each reply, and the
roll events are counted: a still scene must produce exactly one.
"""
from __future__ import annotations

import asyncio
import json
import sys
from pathlib import Path

import websockets

ROOT = Path(__file__).resolve().parent.parent
VAL_IMG = ROOT / "data" / "yolo_det" / "images" / "val"


async def probe(img_path: Path, url: str, repeat: int) -> None:
    data = img_path.read_bytes()
    print(f"sending {img_path.name} ({len(data)} bytes) x{repeat} to {url}")
    frames, rolls = [], []
    async with websockets.connect(url, max_size=None) as ws:
        for i in range(repeat):
            await ws.send(data)
            # Wait for this frame's reply (first one is slow: CubeCL autotunes kernels).
            while True:
                msg = json.loads(await asyncio.wait_for(ws.recv(), timeout=240 if i == 0 else 30))
                if msg.get("type") == "roll":
                    rolls.append(msg)
                    continue
                frames.append(msg)
                break
        # A roll is sent right after the frame that settled it; collect stragglers.
        try:
            while True:
                msg = json.loads(await asyncio.wait_for(ws.recv(), timeout=1))
                (rolls if msg.get("type") == "roll" else frames).append(msg)
        except asyncio.TimeoutError:
            pass
    print("first frame:", json.dumps(frames[0], indent=2))
    for r in rolls:
        print("ROLL:", json.dumps(r))
    print(f"\n{len(frames)} frame replies, {len(rolls)} roll event(s); last frame_ms={frames[-1].get('frame_ms')}")
    assert frames[0].get("detections"), "no detections returned"
    if repeat > 1:
        assert len(rolls) == 1, f"expected exactly one roll for a still scene, got {len(rolls)}"


def main() -> None:
    img = Path(sys.argv[1]) if len(sys.argv) > 1 else sorted(VAL_IMG.glob("*.jpg"))[0]
    url = sys.argv[2] if len(sys.argv) > 2 else "ws://127.0.0.1:9000"
    repeat = int(sys.argv[3]) if len(sys.argv) > 3 else 1
    asyncio.run(probe(img, url, repeat))


if __name__ == "__main__":
    main()
