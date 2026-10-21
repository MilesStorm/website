import type { ReactNode } from 'react';
import { Card, CardAction, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from '@/components/ui/card';

// The game-server panels' cards (Ark, Enshrouded) and the dice reader's last roll, shared with
// their placeholders so the two can't drift apart in size. After the stat cards of shadcn's
// dashboard example: a small muted label, the figure large, a badge beside it and muted lines
// under it.

/** A figure at a glance. `heading`: the label names the section (an h2). `live`: the figure is
 * read out when it changes. */
export function StatCard({
  label,
  heading = false,
  live = false,
  value,
  action,
  footer,
}: {
  label: string;
  heading?: boolean;
  live?: boolean;
  value: ReactNode;
  action?: ReactNode;
  footer?: ReactNode;
}) {
  return (
    <Card>
      <CardHeader>
        <CardDescription>{heading ? <h2>{label}</h2> : label}</CardDescription>
        <CardTitle className="text-3xl font-semibold tabular-nums" aria-live={live ? 'polite' : undefined}>
          {value}
        </CardTitle>
        {action && <CardAction>{action}</CardAction>}
      </CardHeader>
      {footer && <CardFooter className="flex-col items-stretch gap-2 text-sm">{footer}</CardFooter>}
    </Card>
  );
}

/** How full something is (players of the slots), as a thin bar: the figure beside it says it. */
export function Meter({ value, max }: { value: number; max: number }) {
  const share = max > 0 ? Math.min(100, (value / max) * 100) : 0;
  return (
    <div aria-hidden="true" className="h-1.5 w-full overflow-hidden rounded-full bg-muted">
      <div className="h-full rounded-full bg-primary" style={{ width: `${share}%` }} />
    </div>
  );
}

/** A game server's command buttons, and under them what they say (`children`). */
export function ServerControls({
  description,
  buttons,
  children,
}: {
  description?: ReactNode;
  buttons: ReactNode;
  children?: ReactNode;
}) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>
          <h2>Server controls</h2>
        </CardTitle>
        {description && <CardDescription>{description}</CardDescription>}
      </CardHeader>
      <CardContent className="flex flex-col gap-4">
        <div className="flex flex-wrap gap-2">{buttons}</div>
        {children}
      </CardContent>
    </Card>
  );
}
