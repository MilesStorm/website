import type { ReactNode } from 'react';
import { LoaderCircle } from 'lucide-react';
import { Badge } from '@/components/ui/badge';
import { Card, CardDescription, CardFooter, CardHeader, CardTitle } from '@/components/ui/card';
import { Skeleton } from '@/components/ui/skeleton';
import { cn } from '@/lib/utils';

// The game-server panels' cards (Ark, Enshrouded) and the dice reader's last roll, shared with
// their placeholders so the two can't drift apart in size. After the stat cards of shadcn's
// dashboard example: a small muted label, the figure large (smaller in a narrow card), and a muted
// line under it. The figure and each line stay on one line, so a card is as tall loaded as its
// placeholder.

/** The figure's size: by the card's own width, as in shadcn's dashboard (and small enough for a
 * card a third of a phone's width: a seven-digit build still fits at 360px). */
const FIGURE = 'text-lg @[112px]/card:text-xl @[160px]/card:text-2xl @[250px]/card:text-3xl';
/** The figure's place while it loads: its line's height. */
const FIGURE_PLACE = 'h-7 @[160px]/card:h-8 @[250px]/card:h-9';
/** The same for a card that `grow`s: a step larger once the card has the room (a desktop's
 * wide tiles; the phone's and the dashboard's cards stay as they were). */
const FIGURE_GROW = `${FIGURE} @[340px]/card:text-4xl @[480px]/card:text-5xl`;
export const FIGURE_PLACE_GROW = `${FIGURE_PLACE} @[340px]/card:h-10 @[480px]/card:h-12`;

/** A figure at a glance. `shortLabel`: what a phone's narrow card
 * says instead (the label stays its name). `heading`: the label names the section (an h2). `live`: the figure is
 * read out when it changes. `compact`: less padding on a phone, for cards side by side. `grow`: the
 * figure steps up to 4xl in a card 340px wide, 5xl at 480px. `className`: for the card. */
export function StatCard({
  label,
  shortLabel,
  heading = false,
  live = false,
  compact = false,
  grow = false,
  className,
  value,
  footer,
}: {
  label: string;
  shortLabel?: string;
  heading?: boolean;
  live?: boolean;
  compact?: boolean;
  grow?: boolean;
  className?: string;
  value: ReactNode;
  footer?: ReactNode;
}) {
  const text = shortLabel ? (
    <>
      <span aria-hidden="true" className="sm:hidden">
        {shortLabel}
      </span>
      <span className="max-sm:sr-only">{label}</span>
    </>
  ) : (
    label
  );
  return (
    <Card
      className={cn('@container/card', compact && '[--card-spacing:--spacing(3)]', className)}
    >
      <CardHeader>
        <CardDescription>{heading ? <h2>{text}</h2> : text}</CardDescription>
        <CardTitle
          className={`${grow ? FIGURE_GROW : FIGURE} truncate font-semibold tabular-nums`}
          aria-live={live ? 'polite' : undefined}
          // Cut short in a narrow card: the whole figure on hover.
          title={typeof value === 'string' ? value : undefined}
        >
          {value}
        </CardTitle>
      </CardHeader>
      {footer && <CardFooter className="flex-col items-stretch gap-2 text-sm">{footer}</CardFooter>}
    </Card>
  );
}

/** A muted line under a figure, kept to one line (the whole line on hover). `short`: what a
 * phone's narrow card says instead. */
export function StatNote({ children, short }: { children: string; short?: string }) {
  return (
    <p className="truncate text-muted-foreground" title={children}>
      {short ? (
        <>
          <span aria-hidden="true" className="sm:hidden">
            {short}
          </span>
          <span className="max-sm:sr-only">{children}</span>
        </>
      ) : (
        children
      )}
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

/** How a server's state reads: the colour of its dot. */
export type Tone = 'online' | 'pending' | 'off' | 'bad';

const DOTS: Record<Tone, string> = {
  online: 'bg-emerald-500',
  pending: 'bg-amber-500',
  off: 'bg-muted-foreground/60',
  bad: 'bg-destructive',
};

/** A server's state: a dot in the tone's colour and the state's name. `moving`: under way (a
 * spinner in the dot's place, amber like the dot would be). */
export function StatusBadge({ tone, label, moving = false }: { tone: Tone; label: string; moving?: boolean }) {
  return (
    <Badge variant="outline" className="h-6 gap-1.5 px-2.5 text-sm">
      {moving ? (
        <LoaderCircle aria-hidden="true" className="text-amber-500 motion-safe:animate-spin" />
      ) : (
        <span aria-hidden="true" className={`size-2 rounded-full ${DOTS[tone]}`} />
      )}
      {label}
    </Badge>
  );
}

/** The place of a state while it loads. */
export function StatusPlace() {
  return <Skeleton className="h-6 w-20 rounded-4xl" />;
}
