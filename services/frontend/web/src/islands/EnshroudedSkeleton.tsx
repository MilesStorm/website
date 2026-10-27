import type { ReactNode } from 'react';
import { Skeleton } from '@/components/ui/skeleton';
import { ButtonPlace } from '@/components/Placeholders';
import { FIGURE_PLACE, Meter, NotePlace, StatCard, StatNote, StatusPlace } from '@/components/ServerCards';

// The Enshrouded panel's frame, shared by the panel (islands/EnshroudedPanel.tsx) and its
// placeholder, so the two can't drift apart in size: the heading with the state's badge and the
// buttons, and the figures. The notices, who's on and the latest commands come after them, so
// nothing above moves when they do. Free of the API client: the placeholder is rendered when the
// page is built, with a pulse where the server's data and each button go (e2e/enshrouded.spec.ts
// measures it).

const INTRO = 'The Enshrouded server.';
/** The buttons' labels, by the command each asks for. */
export const ENSHROUDED_BUTTONS = { restart: 'Restart', update: 'Force update' };
export const UPTIME_NOTE = 'Since the last start.';
export const UPTIME_NOTE_SHORT = 'Since start';

/** The page's heading: the world's name, and the server's state and buttons beside it (under it
 * on a phone; none when the status couldn't be had). */
export function EnshroudedHeader({ badge, buttons }: { badge?: ReactNode; buttons?: ReactNode }) {
  return (
    <div className="flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between">
      <div className="flex flex-col gap-1">
        <h1 className="scroll-m-20 text-2xl font-semibold tracking-tight">Chronicle</h1>
        <p className="text-muted-foreground">{INTRO}</p>
      </div>
      {badge && (
        <div className="flex flex-wrap items-center gap-2">
          {badge}
          {buttons}
        </div>
      )}
    </div>
  );
}

/** The figures: players of the slots, uptime and the running build. */
export function Figures({ children }: { children: ReactNode }) {
  return <div className="grid grid-cols-3 gap-2 sm:gap-4">{children}</div>;
}

export function EnshroudedSkeleton() {
  return (
    <div role="status" aria-label="Loading the server's status" aria-busy="true" className="flex flex-col gap-6">
      <div data-overview className="flex flex-col gap-6">
        <EnshroudedHeader
          badge={<StatusPlace />}
          buttons={Object.entries(ENSHROUDED_BUTTONS).map(([cmd, label]) => (
            <ButtonPlace key={cmd} label={label} size="default" />
          ))}
        />
        <Figures>
          <StatCard
            compact
            label="Players online"
            shortLabel="Players"
            heading
            value={<Skeleton className={`${FIGURE_PLACE} w-16`} />}
            footer={
              <>
                <Meter value={0} max={1} />
                <NotePlace />
              </>
            }
          />
          <StatCard
            compact
            label="Uptime"
            value={<Skeleton className={`${FIGURE_PLACE} w-16`} />}
            footer={<StatNote short={UPTIME_NOTE_SHORT}>{UPTIME_NOTE}</StatNote>}
          />
          <StatCard compact label="Build" value={<Skeleton className={`${FIGURE_PLACE} w-16`} />} footer={<NotePlace />} />
        </Figures>
      </div>
    </div>
  );
}
