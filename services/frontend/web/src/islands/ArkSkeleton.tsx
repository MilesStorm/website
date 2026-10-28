import type { ReactNode } from 'react';
import { Skeleton } from '@/components/ui/skeleton';
import { ButtonPlace } from '@/components/Placeholders';
import { PageHeader } from '@/components/PageHeader';
import { PAGE_STACK } from '@/lib/layout';
import { FIGURE_PLACE_GROW, NotePlace, StatCard, StatusPlace } from '@/components/ServerCards';
import type { Schemas } from '@/api/client';

// The Ark panel's place while it loads (islands/ArkPanel.tsx): the heading with its state and
// buttons, and the count's card, with a pulse where the state, the count and its line go, so
// nothing moves when it arrives (e2e/profile.spec.ts measures it). Free of the API client:
// rendered when the page is built.

/** The buttons: each command and its label, in order. */
export const ARK_BUTTONS: [Schemas['ArkCommand'], string][] = [
  ['restart', 'Restart'],
  ['stop', 'Stop'],
  ['start', 'Start'],
];

/** The page's heading (PageHeader), with the server's state (none when it couldn't be had) beside
 * the title and the buttons at the right of it, under it on a phone. */
export function ArkHeader({ badge, buttons }: { badge?: ReactNode; buttons: ReactNode }) {
  return (
    <PageHeader
      title="Ark server"
      description="Who's online, and start, stop or restart the server."
      meta={badge}
      actions={buttons}
    />
  );
}

/** The player count's card: pulses while `loading`, else the count and its line. The line's region
 * is there from the start, so the first count is read out too (`live`). */
export function PlayersCard({
  loading,
  live = false,
  value,
  note,
}: {
  loading: boolean;
  live?: boolean;
  value?: ReactNode;
  note?: ReactNode;
}) {
  return (
    <StatCard
      label="Players online"
      grow
      // One figure: a card of its own measure, not stretched across the container.
      className="max-w-md"
      heading
      value={loading ? <Skeleton className={`${FIGURE_PLACE_GROW} w-16`} /> : value}
      footer={<div aria-live={live ? 'polite' : undefined}>{loading ? <NotePlace className="w-40" /> : note}</div>}
    />
  );
}


export function ArkSkeleton() {
  return (
    <div role="status" aria-label="Loading the player count" aria-busy="true" className={PAGE_STACK}>
      <ArkHeader
        badge={<StatusPlace />}
        buttons={ARK_BUTTONS.map(([cmd, label]) => (
          <ButtonPlace key={cmd} label={label} size="default" />
        ))}
      />
      <PlayersCard loading />
    </div>
  );
}
