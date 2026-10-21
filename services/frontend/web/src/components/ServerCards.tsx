import type { ReactNode } from 'react';
import { Card, CardAction, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from '@/components/ui/card';
import { Skeleton } from '@/components/ui/skeleton';

// The game-server panels' cards (Ark, Enshrouded) and the dice reader's last roll, shared with
// their placeholders so the two can't drift apart in size. After the stat cards of shadcn's
// dashboard example: a small muted label, the figure large (smaller in a narrow card), a badge
// beside it and a muted line under it. The figure and each line stay on one line, so a card is as
// tall loaded as its placeholder.

/** The figure's size: by the card's own width, as in shadcn's dashboard. */
const FIGURE = 'text-2xl @[250px]/card:text-3xl';
/** The figure's place while it loads: its line's height. */
export const FIGURE_PLACE = 'h-8 @[250px]/card:h-9';

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
    <Card className="@container/card">
      <CardHeader>
        <CardDescription>{heading ? <h2>{label}</h2> : label}</CardDescription>
        <CardTitle
          className={`${FIGURE} truncate font-semibold tabular-nums`}
          aria-live={live ? 'polite' : undefined}
          // Cut short in a narrow card: the whole figure on hover.
          title={typeof value === 'string' ? value : undefined}
        >
          {value}
        </CardTitle>
        {action && <CardAction>{action}</CardAction>}
      </CardHeader>
      {footer && <CardFooter className="flex-col items-stretch gap-2 text-sm">{footer}</CardFooter>}
    </Card>
  );
}

/** A muted line under a figure, kept to one line (the whole line on hover). */
export function StatNote({ children }: { children: string }) {
  return (
    <p className="truncate text-muted-foreground" title={children}>
      {children}
    </p>
  );
}

/** A pulse for a figure's muted line. */
export function NotePlace({ className = 'w-28' }: { className?: string }) {
  return <Skeleton className={`h-5 ${className}`} />;
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

/** A game server's command buttons; under them a line on why they wait (`note`), and what came of
 * the last command (`children`), read out as it changes. */
export function ServerControls({
  description,
  buttons,
  note,
  children,
}: {
  description?: ReactNode;
  buttons: ReactNode;
  note?: ReactNode;
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
        {note}
        <div aria-live="polite">{children}</div>
      </CardContent>
    </Card>
  );
}
