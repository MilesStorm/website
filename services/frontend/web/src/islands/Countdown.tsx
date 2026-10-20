import { useEffect, useState } from 'react';

const DAY = 86_400;

/** Days, hours, minutes and seconds in `secs`. */
export function split(secs: number): [number, number, number, number] {
  return [Math.floor(secs / DAY), Math.floor((secs % DAY) / 3600), Math.floor((secs % 3600) / 60), secs % 60];
}

/** A countdown to a random moment 30 to 365 days away; a draw under 30 days reloads the page. */
export function Countdown({ random = Math.random }: { random?: () => number }) {
  const [left, setLeft] = useState<number | null>(null);
  useEffect(() => {
    const days = 1 + Math.floor(random() * 365);
    if (days < 30) {
      location.reload();
      return;
    }
    const target = Date.now() + (days * DAY + Math.floor(random() * DAY)) * 1000;
    const tick = () => setLeft(Math.max(0, Math.floor((target - Date.now()) / 1000)));
    tick();
    const timer = setInterval(tick, 1000);
    return () => clearInterval(timer);
  }, [random]);
  if (left === null) return <p className="text-sm text-muted-foreground">calculating...</p>;
  const [d, h, m, s] = split(left);
  return (
    <p className="font-mono text-4xl tabular-nums" role="timer">
      {d}d {h}h {m}m {s}s
    </p>
  );
}
