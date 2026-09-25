// Roll events as sent by ai_pipeline (src/roll.rs) through the website:
//   {"type":"roll","roll_id":"..","dice":[{"value":"17"|null,"conf":0.93,"box":[x1,y1,x2,y2]}],
//    "total":17|null,"complete":true,"ts":1727250000000}
// Dice are ordered left to right. `value` is null when the die could not be read
// confidently; the server never guesses, and then `total` is null too.

/** Parse and validate one roll event's JSON; null if it isn't a usable roll. */
export function parseRoll(text) {
  let v;
  try {
    v = JSON.parse(text);
  } catch {
    return null;
  }
  if (!v || v.type !== "roll" || typeof v.roll_id !== "string" || !Array.isArray(v.dice)) return null;
  const dice = v.dice.map((d) => ({
    value: typeof d?.value === "string" ? d.value : null,
    conf: typeof d?.conf === "number" ? d.conf : 0,
  }));
  return {
    rollId: v.roll_id,
    dice,
    total: Number.isInteger(v.total) ? v.total : null,
    complete: v.complete === true && dice.every((d) => d.value !== null),
    ts: Number.isFinite(v.ts) ? v.ts : Date.now(),
  };
}

/** Text for the panel: one label per die ("?" if unreadable), the total, and how many were unreadable. */
export function describeRoll(roll) {
  const dice = roll.dice.map((d) => ({ label: d.value ?? "?", readable: d.value !== null }));
  const unreadable = dice.filter((d) => !d.readable).length;
  return {
    dice,
    total: unreadable === 0 && roll.total !== null ? String(roll.total) : null,
    unreadable,
  };
}

/** "just now", "42 s ago", "3 min ago", "2 h ago". */
export function timeAgo(ts, now = Date.now()) {
  const s = Math.max(0, Math.round((now - ts) / 1000));
  if (s < 5) return "just now";
  if (s < 60) return `${s} s ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m} min ago`;
  return `${Math.floor(m / 60)} h ago`;
}
