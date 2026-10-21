import type { ReactNode } from 'react';
import { Skeleton } from '@/components/ui/skeleton';
import { ButtonPlace } from '@/components/Placeholders';
import { FIGURE_PLACE, NotePlace, ServerControls, StatCard } from '@/components/ServerCards';
import type { Schemas } from '@/api/client';

// The Ark panel's place while it loads (islands/ArkPanel.tsx): its two cards, with a pulse where
// the count, its badge and its line go and for each button, so nothing moves when it arrives
// (e2e/profile.spec.ts measures it). Free of the API client: rendered when the page is built.

/** The buttons: each command and its label, in Dioxus's order. */
export const ARK_BUTTONS: [Schemas['ArkCommand'], string][] = [
  ['restart', 'Restart'],
  ['stop', 'Stop'],
  ['start', 'Start'],
];

/** The player count's card: pulses while `loading`, else the count, its badge and its line. The
 * line's region is there from the start, so the first count is read out too (`live`). */
export function PlayersCard({
  loading,
  live = false,
  value,
  action,
  note,
}: {
  loading: boolean;
  live?: boolean;
  value?: ReactNode;
  action?: ReactNode;
  note?: ReactNode;
}) {
  return (
    <StatCard
      label="Players online"
      heading
      value={loading ? <Skeleton className={`${FIGURE_PLACE} w-16`} /> : value}
      action={loading ? <Skeleton className="h-5 w-16 rounded-4xl" /> : action}
      footer={<div aria-live={live ? 'polite' : undefined}>{loading ? <NotePlace className="w-40" /> : note}</div>}
    />
  );
}

export function ArkSkeleton() {
  return (
    <div role="status" aria-label="Loading the player count" aria-busy="true" className="flex flex-col gap-6">
      <PlayersCard loading />
      <ServerControls
        buttons={ARK_BUTTONS.map(([cmd, label]) => (
          <ButtonPlace key={cmd} label={label} size="default" />
        ))}
      />
    </div>
  );
}
