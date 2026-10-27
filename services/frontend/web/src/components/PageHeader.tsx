import type { ReactNode } from 'react';
import { cn } from '@/lib/utils';

/** A page's heading, in shadcn's typography: the title, a muted line under it, `meta` beside the
 * title (a status badge) and `actions` (buttons) at the right on `sm` and up, in a row of their
 * own under the text on a phone, where each button takes its share of the row. Page.astro draws
 * it from a page's title and description; an island whose heading carries live data (Ark's,
 * Enshrouded's) draws it with its badge and buttons, and so do its placeholders, so every
 * page's heading is the same markup and the same size. */
export function PageHeader({
  title,
  description,
  meta,
  actions,
  className,
}: {
  title: ReactNode;
  description?: ReactNode;
  meta?: ReactNode;
  actions?: ReactNode;
  className?: string;
}) {
  return (
    <div className={cn('flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between', className)}>
      <div className="flex min-w-0 flex-col gap-1">
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
          <h1 className="scroll-m-20 text-2xl font-semibold tracking-tight sm:text-3xl">{title}</h1>
          {meta}
        </div>
        {description && <p className="text-sm text-muted-foreground sm:text-base">{description}</p>}
      </div>
      {actions && (
        <div className="flex flex-wrap items-center gap-2 max-sm:*:flex-1 sm:justify-end sm:pt-1">{actions}</div>
      )}
    </div>
  );
}
