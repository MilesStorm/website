import type { ReactNode } from 'react';
import { Skeleton } from '@/components/ui/skeleton';
import { ButtonPlace } from '@/components/Placeholders';
import { PageHeader } from '@/components/PageHeader';
import { PAGE_STACK } from '@/lib/layout';
import { FIGURE_PLACE_GROW, Meter, NotePlace, StatCard, StatNote, StatusPlace } from '@/components/ServerCards';

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

/** The page's heading (PageHeader): the world's name, and the server's state beside it and the
 * buttons at the right of it (under it on a phone; none when the status couldn't be had). */
export function EnshroudedHeader({ badge, buttons }: { badge?: ReactNode; buttons?: ReactNode }) {
  return <PageHeader title="Chronicle" description={INTRO} meta={badge} actions={badge && buttons} />;
}

/** The figures: players of the slots, uptime and the running build. Their gap from `lg` is the
 * grid's under them (who's on over two of these columns, the commands over the third). */
export function Figures({ children }: { children: ReactNode }) {
  return <div className="grid grid-cols-3 gap-2 sm:gap-4 lg:gap-8">{children}</div>;
}

export function EnshroudedSkeleton() {
  return (
    <div role="status" aria-label="Loading the server's status" aria-busy="true" className={PAGE_STACK}>
      <div data-overview className={PAGE_STACK}>
        <EnshroudedHeader
          badge={<StatusPlace />}
          buttons={Object.entries(ENSHROUDED_BUTTONS).map(([cmd, label]) => (
            <ButtonPlace key={cmd} label={label} size="default" />
          ))}
        />
        <Figures>
          <StatCard
            compact
            grow
            label="Players online"
            shortLabel="Players"
            heading
            value={<Skeleton className={`${FIGURE_PLACE_GROW} w-16`} />}
            footer={
              <>
                <Meter value={0} max={1} />
                <NotePlace />
              </>
            }
          />
          <StatCard
            compact
            grow
            label="Uptime"
            value={<Skeleton className={`${FIGURE_PLACE_GROW} w-16`} />}
            footer={<StatNote short={UPTIME_NOTE_SHORT}>{UPTIME_NOTE}</StatNote>}
          />
          <StatCard compact grow label="Build" value={<Skeleton className={`${FIGURE_PLACE_GROW} w-16`} />} footer={<NotePlace />} />
        </Figures>
      </div>
    </div>
  );
}
