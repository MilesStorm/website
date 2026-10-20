// The Arcane camera page's rules that aren't the browser plumbing: what the server's messages say,
// how detections are drawn, and the page's own timings (the debug panel).
import type { Schemas } from '@/api/client';

export type ServerMsg = Schemas['ArcaneServerMsg'];
export type Detection = Schemas['Detection'];
export type Roll = Schemas['Roll'];

/** The newest signed Firefox extension (published by .github/workflows/extension.yml). */
export const EXTENSION_XPI = 'https://github.com/MilesStorm/website/releases/download/arcane-dice/arcane-dice.xpi';

/** The camera's frame interval (30 a second): the page never sends frames faster. */
export const CAMERA_FRAME_MS = 33;
/** How long the page waits for the answer to a frame before sending the next one anyway (a frame
 * the server dropped gets none). */
export const REPLY_WAIT_MS = 250;

/** A message from `/ws/arcane`, or `null` for anything that isn't one (the types come from the
 * spec, which the contract fixtures check). */
export function readMessage(text: string): ServerMsg | null {
  try {
    const msg = JSON.parse(text) as ServerMsg;
    return msg && (msg.type === 'frame' || msg.type === 'roll' || msg.type === 'error') ? msg : null;
  } catch {
    return null;
  }
}

/** "5 + 12 = 17", or "5 + ?" when a die could not be read (no total then). */
export function rollText(roll: Pick<Roll, 'dice' | 'total'>): string {
  const dice = roll.dice.map((d) => d.value ?? '?');
  const text = dice.join(' + ');
  return roll.total !== null && dice.length > 1 ? `${text} = ${roll.total}` : text;
}

/** A box's colour: grey when not read confidently; in debug mode by the reading's confidence
 * (green ≥ 80 %, yellow ≥ 50 %, red below); green otherwise. */
export function boxColor(d: Detection, debug: boolean): string {
  if (!d.confident) return 'rgba(156,163,175,0.95)';
  if (!debug) return 'rgba(52,211,153,0.95)';
  if (d.dice_conf >= 0.8) return 'rgba(52,211,153,0.95)';
  if (d.dice_conf >= 0.5) return 'rgba(251,191,36,0.95)';
  return 'rgba(239,68,68,0.95)';
}

const pct = (x: number) => `${(x * 100).toFixed(0)}%`;

/** A box's label: below the confidence threshold the value isn't shown as a guess. */
export function boxLabel(d: Detection, debug: boolean): string {
  if (debug) return `${d.value} ${pct(d.dice_conf)} | y:${pct(d.yolo_conf)}`;
  return d.confident ? `${d.value} ${pct(d.dice_conf)}` : '?';
}

/** One line per detection for the debug panel. */
export function detectionLines(dets: Detection[]): string {
  const f = (x: number) => x.toFixed(3);
  return dets
    .map(
      (d, i) =>
        `det[${i}] dice=${d.value} (${(d.dice_conf * 100).toFixed(1)}%)  yolo_cls=${d.yolo_class} ` +
        `yolo_conf=${(d.yolo_conf * 100).toFixed(1)}%  bbox=[${f(d.x1)},${f(d.y1)} → ${f(d.x2)},${f(d.y2)}]`,
    )
    .join('\n');
}

/** Clears the overlay and draws each detection's box and label (sizes in the video's pixels). */
export function drawOverlay(ctx: CanvasRenderingContext2D, dets: Detection[], w: number, h: number, debug: boolean): void {
  ctx.clearRect(0, 0, w, h);
  for (const d of dets) {
    const x = d.x1 * w;
    const y = d.y1 * h;
    const color = boxColor(d, debug);
    ctx.strokeStyle = color;
    ctx.lineWidth = 2.5;
    ctx.strokeRect(x, y, (d.x2 - d.x1) * w, (d.y2 - d.y1) * h);
    const label = boxLabel(d, debug);
    ctx.font = 'bold 13px monospace';
    ctx.fillStyle = 'rgba(0,0,0,0.55)';
    ctx.fillRect(x, y - 18, label.length * 8 + 6, 17);
    ctx.fillStyle = color;
    ctx.fillText(label, x + 3, y - 4);
  }
}

/** Where a frame's time goes on the page's side, averaged over a second: frames sent a second, the
 * time to encode one, the time from sending one to its answer, and its size. */
export class PageTimings {
  private since = 0;
  private frames = 0;
  private encodeMs = 0;
  private replyMs = 0;
  private bytes = 0;

  /** Counts a frame sent at `now`; `replyMs` is the answer time of the frame before it. */
  frame(now: number, encodeMs: number, replyMs: number, bytes: number): void {
    if (this.frames === 0) this.since = now;
    this.frames += 1;
    this.encodeMs += encodeMs;
    this.replyMs += replyMs;
    this.bytes += bytes;
  }

  /** The averages as text once a second has passed, starting the next second. */
  report(now: number, width: number, height: number): string | null {
    const elapsed = now - this.since;
    if (elapsed < 1000) return null;
    const n = this.frames;
    const text =
      `${(((n - 1) * 1000) / elapsed).toFixed(1)} fps · encode ${(this.encodeMs / n).toFixed(0)} ms · ` +
      `reply ${(this.replyMs / n).toFixed(0)} ms · ${width}x${height} ${(this.bytes / n / 1024).toFixed(0)} kB`;
    this.since = this.frames = this.encodeMs = this.replyMs = this.bytes = 0;
    return text;
  }
}
