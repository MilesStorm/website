import { useEffect, useState } from 'react';
import { Card, CardContent } from '@/components/ui/card';

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
  if (left === null) return <CountdownSkeleton />;
  return <Tiles values={split(left)} />;
}

const UNITS = ['Days', 'Hours', 'Minutes', 'Seconds'];

/** The four figures in cards, or a pulse in each while there are none. */
function Tiles({ values }: { values: number[] | null }) {
  return (
    <div role={values ? 'timer' : undefined} className="grid w-full max-w-xl grid-cols-4 gap-2 sm:gap-4">
      {UNITS.map((unit, i) => (
        <Card key={unit} size="sm">
          <CardContent className="flex flex-col items-center gap-1">
            <span className="block h-10 text-3xl leading-10 font-semibold tracking-tight tabular-nums sm:h-12 sm:text-5xl sm:leading-12">
              {values ? values[i] : <span className="inline-block h-full w-10 animate-pulse rounded-xl bg-muted align-top sm:w-14" />}
            </span>
            <span className="text-xs text-muted-foreground">{unit}</span>
          </CardContent>
        </Card>
      ))}
    </div>
  );
}

/** The countdown's place until it has drawn its moment: the same cards, pulsing. */
export function CountdownSkeleton() {
  return (
    <div role="status" aria-label="Loading" aria-busy="true" className="flex w-full justify-center">
      <Tiles values={null} />
    </div>
  );
}
