import type { ReactNode } from 'react';
import { cn } from '@/lib/utils';

// Pieces of the pages' placeholders (the islands' *Skeleton files), shared between them.

/** A button's place: a pulse as wide as its label (`sm`: h-8, `default`: h-9). */
export function ButtonPlace({ label, size = 'sm', className }: { label: ReactNode; size?: 'sm' | 'default'; className?: string }) {
  return (
    <span
      aria-hidden="true"
      className={cn(
        'inline-flex shrink-0 animate-pulse items-center rounded-4xl border border-transparent bg-muted px-3 text-sm font-medium whitespace-nowrap text-transparent select-none',
        size === 'sm' ? 'h-8' : 'h-9',
        className,
      )}
    >
      {label}
    </span>
  );
}
