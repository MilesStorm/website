// The start page's storm in motion: drops of light falling down the dot grid, a branching strike
// every so often, and the pointer parting the dots around it. Draws the same grid and fade as the
// CSS still frame (styles/storm.css), which it replaces only while motion is allowed and it is on
// screen. Everything fades with the dot's own mask, so nothing is bright at the edges.
import { motionOn } from '@/lib/theme';

/** The mask's core: full strength out to this share of the ellipse's radii, around the hero's words... */
const CORE = 0.2;
/** ...easing down to `FIELD` of it here (the dots away from the words, still plainly there)... */
const SHOULDER = 0.65;
const FIELD = 0.4;
/** ...then to nothing at this share. */
const REACH = 0.95;
const STEPS = 6;

const smooth = (t: number) => t * t * (3 - 2 * t);

/** The mask as stops, [share of the radii, opacity]: the core, then two smoothstep falls in `STEPS`
 * pieces each, so the dots are brightest round the words and the field has no edge. The CSS mask
 * (`maskImage`) is drawn from these and the canvas (`maskAt`) interpolates the same ones, so the
 * still frame and the live storm are the same picture. */
const MASK: [number, number][] = [[0, 1], [CORE, 1]];
for (const [from, to, a0, a1] of [
  [CORE, SHOULDER, 1, FIELD],
  [SHOULDER, REACH, FIELD, 0],
]) {
  for (let i = 1; i <= STEPS; i++) {
    const t = i / STEPS;
    MASK.push([from + (to - from) * t, a0 + (a1 - a0) * smooth(t)]);
  }
}

/** The CSS mask for the still frame's dots (`mask-image`): an ellipse as tall as the box and
 * `--storm-wide` times as wide (styles/storm.css; wider than the box on a phone, so the field's
 * sides fall off past the screen's edges, not inside them). */
export const maskImage = `radial-gradient(calc(50% * var(--storm-wide)) 50% at 50% 50%, ${MASK.map(
  ([at, a]) => `rgb(0 0 0 / ${a.toFixed(4)}) ${(at * 100).toFixed(2)}%`,
).join(', ')})`;

/** The mask's opacity at `r`, the distance from the centre in radii (the ellipse's: 1 at its edge). */
function mask(r: number) {
  if (r >= REACH) return 0;
  for (let i = 1; i < MASK.length; i++) {
    if (r <= MASK[i][0]) {
      const [x0, a0] = MASK[i - 1];
      const [x1, a1] = MASK[i];
      return a0 + ((a1 - a0) * (r - x0)) / (x1 - x0);
    }
  }
  return 0;
}

const GRID = 18;
const FALLOFF = 170;
const BUCKETS = 24;

interface Dot {
  x: number;
  y: number;
  /** The CSS mask's opacity here: 1 in the core, 0 from the reach. */
  m: number;
}

interface Drop {
  col: number;
  y: number;
  speed: number;
  len: number;
}

/** x1, y1, x2, y2, line width */
type Segment = [number, number, number, number, number];

/** Starts it on `root`; the result stops it. */
export function startStorm(root: HTMLElement): () => void {
  const canvas = root.querySelector('canvas');
  const flash = root.querySelector<HTMLElement>('.storm-flash');
  const ctx = canvas?.getContext('2d');
  if (!canvas || !flash || !ctx) return () => {};

  let width = 0;
  let height = 0;
  let dots: Dot[] = [];
  let cols: number[][] = [];
  let glow = new Float32Array(0);
  let shiftX = new Float32Array(0);
  let shiftY = new Float32Array(0);
  let lit = new Float32Array(0);
  let drops: Drop[] = [];
  let bolt: Segment[] = [];
  let struck = -Infinity;
  let nextStrike = 0;
  let color = '';
  let dotAlpha = 0;
  let dotRadius = 1;
  // CSS px per storm px: the root font size over 16 (global.css scales it up on wide screens), so
  // the canvas draws in the still frame's units (rem) and the grid, drops and strikes grow with it.
  let unit = 1;
  // The mask's width over the box's (`--storm-wide`).
  let wide = 1;
  let strength = 0;
  const pointer = { x: 0, y: 0, on: false };
  const eased = { x: 0, y: 0 };
  let frame = 0;
  let last = 0;
  let visible = false;

  // The foreground and strengths come from the same custom properties the CSS still frame uses.
  function readStyle() {
    const style = getComputedStyle(root);
    color = style.getPropertyValue('--foreground').trim();
    dotAlpha = parseFloat(style.getPropertyValue('--storm-dot')) / 100;
    unit = parseFloat(getComputedStyle(document.documentElement).fontSize) / 16 || 1;
    // In rem (the still frame's), so in storm px: 16 per rem.
    dotRadius = parseFloat(style.getPropertyValue('--storm-dot-r')) * 16;
    strength = parseFloat(style.getPropertyValue('--storm-lit'));
    wide = parseFloat(style.getPropertyValue('--storm-wide')) || 1;
  }

  // The CSS mask: an ellipse as tall as the box and `wide` times as wide, from the same stops.
  function maskAt(x: number, y: number) {
    return mask(Math.hypot((x - width / 2) / ((width / 2) * wide), (y - height / 2) / (height / 2)));
  }

  function layout() {
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    width = canvas!.clientWidth / unit;
    height = canvas!.clientHeight / unit;
    canvas!.width = canvas!.clientWidth * dpr;
    canvas!.height = canvas!.clientHeight * dpr;
    ctx!.setTransform(dpr * unit, 0, 0, dpr * unit, 0, 0);
    dots = [];
    cols = [];
    for (let x = GRID / 2; x < width; x += GRID) {
      const col: number[] = [];
      for (let y = GRID / 2; y < height; y += GRID) {
        col.push(dots.length);
        dots.push({ x, y, m: maskAt(x, y) });
      }
      cols.push(col);
    }
    glow = new Float32Array(dots.length);
    shiftX = new Float32Array(dots.length);
    shiftY = new Float32Array(dots.length);
    lit = new Float32Array(dots.length);
    drops = [];
    bolt = [];
  }

  function rain(dt: number) {
    for (let n = cols.length * 0.28 * dt + Math.random(); n >= 1; n--) {
      drops.push({
        col: Math.floor(Math.random() * cols.length),
        y: -20 - Math.random() * 60,
        speed: 380 + Math.random() * 260,
        len: 30 + Math.random() * 60,
      });
    }
    for (const drop of drops) {
      drop.y += drop.speed * dt;
      for (const k of cols[drop.col]) {
        const behind = drop.y - dots[k].y;
        if (behind >= 0 && behind < drop.len * 3) glow[k] += Math.exp(-behind / drop.len) * 0.9;
        else if (behind < 0 && behind > -9) glow[k] += Math.exp(-(behind * behind) / 20) * 0.9;
      }
    }
    drops = drops.filter((drop) => drop.y < height + drop.len * 3);
  }

  // Midpoint displacement: split each segment at a jittered middle until the jitter is small.
  function branch(x1: number, y1: number, x2: number, y2: number, jitter: number, w: number) {
    if (jitter < 3) {
      bolt.push([x1, y1, x2, y2, w]);
      return;
    }
    const mx = (x1 + x2) / 2 + (Math.random() - 0.5) * jitter;
    const my = (y1 + y2) / 2 + (Math.random() - 0.5) * jitter * 0.3;
    branch(x1, y1, mx, my, jitter / 2, w);
    branch(mx, my, x2, y2, jitter / 2, w);
  }

  function strike(now: number) {
    bolt = [];
    const x0 = width / 2 + (Math.random() - 0.5) * width * 0.4;
    branch(
      x0,
      height * 0.05,
      x0 + (Math.random() - 0.5) * height * 0.5,
      height * (0.55 + Math.random() * 0.3),
      height * 0.4,
      1.6,
    );
    const trunk = bolt.length;
    for (let b = 2 + Math.floor(Math.random() * 3); b > 0; b--) {
      const [, , x, y] = bolt[Math.floor(trunk * (0.2 + Math.random() * 0.5))];
      const side = Math.random() < 0.5 ? -1 : 1;
      branch(
        x,
        y,
        x + side * height * (0.15 + Math.random() * 0.3),
        y + height * (0.18 + Math.random() * 0.3),
        height * 0.15,
        0.8,
      );
    }
    for (let k = 0; k < dots.length; k++) {
      let nearest = Infinity;
      for (const s of bolt) nearest = Math.min(nearest, (dots[k].x - s[2]) ** 2 + (dots[k].y - s[3]) ** 2);
      lit[k] = Math.exp(-Math.sqrt(nearest) / 70);
    }
    struck = now;
    nextStrike = now + 15000 + Math.random() * 25000;
  }

  // A double flicker, then a fade.
  function flashAt(now: number) {
    const t = now - struck;
    if (t < 0) return 0;
    if (t < 70) return 1;
    if (t < 140) return 0.25;
    if (t < 230) return 0.9;
    const e = 0.9 * Math.exp(-(t - 230) / 350);
    return e < 0.01 ? 0 : e;
  }

  function draw(now: number) {
    const dt = Math.min(50, now - last) / 1000;
    last = now;
    ctx!.clearRect(0, 0, width, height);
    glow.fill(0);
    shiftX.fill(0);
    shiftY.fill(0);

    if (now > nextStrike) strike(now);
    const light = flashAt(now);
    flash!.style.opacity = String(light);
    rain(dt);
    if (light > 0) for (let k = 0; k < dots.length; k++) glow[k] += light * (lit[k] * 1.3 + 0.12);
    if (pointer.on) {
      eased.x += (pointer.x - eased.x) * 0.08;
      eased.y += (pointer.y - eased.y) * 0.08;
      for (let k = 0; k < dots.length; k++) {
        const dx = dots[k].x - eased.x;
        const dy = dots[k].y - eased.y;
        const d = Math.hypot(dx, dy);
        if (d < FALLOFF && d > 0.01) {
          const q = (1 - d / FALLOFF) ** 2;
          glow[k] += q * 1.1;
          shiftX[k] = (dx / d) * q * 7;
          shiftY[k] = (dy / d) * q * 7;
        }
      }
    }

    // Grouped by alpha so the fill changes a few dozen times per frame, not once per dot.
    const buckets: number[][] = Array.from({ length: BUCKETS }, () => []);
    for (let k = 0; k < dots.length; k++) {
      const m = dots[k].m;
      const a = Math.min(1, m * (dotAlpha + Math.min(glow[k], 1.4) * strength));
      if (a >= 0.004) buckets[Math.min(BUCKETS - 1, Math.floor(a * BUCKETS))].push(k);
    }
    ctx!.fillStyle = color;
    for (let b = 0; b < BUCKETS; b++) {
      if (!buckets[b].length) continue;
      ctx!.globalAlpha = (b + 0.5) / BUCKETS;
      ctx!.beginPath();
      for (const k of buckets[b]) {
        const x = dots[k].x + shiftX[k];
        const y = dots[k].y + shiftY[k];
        const r = dotRadius * (1 + Math.min(glow[k], 1.4) * 0.7);
        ctx!.moveTo(x + r, y);
        ctx!.arc(x, y, r, 0, Math.PI * 2);
      }
      ctx!.fill();
    }

    if (light > 0.02) {
      ctx!.save();
      ctx!.lineCap = 'round';
      ctx!.strokeStyle = color;
      ctx!.shadowColor = color;
      ctx!.shadowBlur = 14 * unit;
      for (const [x1, y1, x2, y2, w] of bolt) {
        ctx!.globalAlpha = light * (w > 1 ? 1.1 : 0.6) * strength * maskAt((x1 + x2) / 2, (y1 + y2) / 2);
        ctx!.lineWidth = w;
        ctx!.beginPath();
        ctx!.moveTo(x1, y1);
        ctx!.lineTo(x2, y2);
        ctx!.stroke();
      }
      ctx!.restore();
    }
    ctx!.globalAlpha = 1;
    frame = requestAnimationFrame(draw);
  }

  function run() {
    const live = motionOn() && visible && !document.hidden;
    root.toggleAttribute('data-live', live);
    if (live && !frame) {
      readStyle();
      layout();
      last = performance.now();
      nextStrike = Math.max(nextStrike, last + 4000 + Math.random() * 4000);
      frame = requestAnimationFrame(draw);
    } else if (!live && frame) {
      cancelAnimationFrame(frame);
      frame = 0;
      flash!.style.opacity = '0';
    }
  }

  // Behind the page's content with pointer-events off, so it listens on the window.
  const off = new AbortController();
  const { signal } = off;
  window.addEventListener(
    'pointermove',
    (e) => {
      if (e.pointerType === 'touch') return;
      const rect = canvas.getBoundingClientRect();
      pointer.x = (e.clientX - rect.left) / unit;
      pointer.y = (e.clientY - rect.top) / unit;
      if (!pointer.on) {
        eased.x = pointer.x;
        eased.y = pointer.y;
      }
      pointer.on = pointer.x >= 0 && pointer.y >= 0 && pointer.x <= width && pointer.y <= height;
    },
    { signal },
  );
  document.documentElement.addEventListener('pointerleave', () => (pointer.on = false), { signal });

  const seen = new IntersectionObserver(([entry]) => {
    visible = entry.isIntersecting;
    run();
  });
  seen.observe(root);
  // A new width can mean a new root size (global.css), so the units are read again.
  const sized = new ResizeObserver(() => {
    if (!frame) return;
    readStyle();
    layout();
  });
  sized.observe(canvas);
  // The theme script toggles `.dark` on <html>, and sets `data-motion` for a motion choice.
  const themed = new MutationObserver(() => {
    readStyle();
    run();
  });
  themed.observe(document.documentElement, { attributeFilter: ['class', 'data-motion'] });
  matchMedia('(color-gamut: p3)').addEventListener('change', readStyle, { signal });
  matchMedia('(prefers-reduced-motion: reduce)').addEventListener('change', run, { signal });
  document.addEventListener('visibilitychange', run, { signal });

  return () => {
    off.abort();
    seen.disconnect();
    sized.disconnect();
    themed.disconnect();
    cancelAnimationFrame(frame);
    root.removeAttribute('data-live');
  };
}
