import type { ReactNode } from 'react';
import { Skeleton } from '@/components/ui/skeleton';
import { Meter, ServerControls, StatCard } from '@/components/ServerCards';
import { ButtonPlace } from '@/islands/ProfileSkeleton';

// The Enshrouded panel's frame, shared by the panel (islands/EnshroudedPanel.tsx) and its
// placeholder, so the two can't drift apart in size: the heading with the state's badge, the
// figures, and the controls. The notices, who's on and the latest commands come after them, so
// nothing above moves when they do. Free of the API client: the placeholder is rendered when the
// page is built, with a pulse where the server's data and each button go (e2e/enshrouded.spec.ts
// measures it).

export const ENSHROUDED_INTRO = 'The Enshrouded server.';
/** The buttons: Restart, then Force update. */
export const ENSHROUDED_BUTTONS = ['Restart', 'Force update'] as const;
export const UPTIME_NOTE = 'Since the last start.';

/** The page's heading: the world's name, and the server's state beside it. */
export function EnshroudedHeader({ badge }: { badge: ReactNode }) {
  return (
    <div className="flex items-start justify-between gap-4">
      <div className="flex flex-col gap-1">
        <h1 className="scroll-m-20 text-2xl font-semibold tracking-tight">Chronicle</h1>
        <p className="text-muted-foreground">{ENSHROUDED_INTRO}</p>
      </div>
      <div className="pt-1.5">{badge}</div>
    </div>
  );
}

/** The figures: players of the slots, uptime and the running build. */
export function Figures({ children }: { children: ReactNode }) {
  return <div className="grid gap-4 sm:grid-cols-3">{children}</div>;
}

export function EnshroudedSkeleton() {
  const line = <Skeleton className="h-5 w-28" />;
  return (
    <div role="status" aria-label="Loading the server's status" aria-busy="true" className="flex flex-col gap-6">
      <div data-overview className="flex flex-col gap-6">
        <EnshroudedHeader badge={<Skeleton className="h-5 w-16 rounded-4xl" />} />
        <Figures>
          <StatCard
            label="Players online"
            heading
            value={<Skeleton className="h-9 w-20" />}
            footer={
              <>
                <Meter value={0} max={1} />
                {line}
              </>
            }
          />
          <StatCard
            label="Uptime"
            value={<Skeleton className="h-9 w-24" />}
            footer={<p className="text-muted-foreground">{UPTIME_NOTE}</p>}
          />
          <StatCard label="Build" value={<Skeleton className="h-9 w-28" />} footer={line} />
        </Figures>
        <ServerControls
          buttons={ENSHROUDED_BUTTONS.map((label) => (
            <ButtonPlace key={label} label={label} size="default" />
          ))}
        >
          <div />
        </ServerControls>
      </div>
    </div>
  );
}
