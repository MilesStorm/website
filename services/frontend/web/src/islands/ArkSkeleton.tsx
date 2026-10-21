import { Skeleton } from '@/components/ui/skeleton';
import { ServerControls, StatCard } from '@/components/ServerCards';
import { ButtonPlace } from '@/islands/ProfileSkeleton';

// The Ark panel's place while it loads (islands/ArkPanel.tsx): its two cards, with a pulse where
// the count, its badge and its line go and for each button, so nothing moves when it arrives
// (e2e/profile.spec.ts measures it). Free of the API client: rendered when the page is built.

/** The buttons, in Dioxus's order. */
export const ARK_BUTTONS = ['Restart', 'Stop', 'Start'] as const;

/** The player count's card while the count loads (also the panel's, before its first answer). */
export function PlayersPlace() {
  return (
    <StatCard
      label="Players online"
      heading
      value={<Skeleton className="h-9 w-16" />}
      action={<Skeleton className="h-5 w-16 rounded-4xl" />}
      footer={<Skeleton className="h-5 w-40" />}
    />
  );
}

export function ArkSkeleton() {
  return (
    <div role="status" aria-label="Loading the player count" aria-busy="true" className="flex flex-col gap-6">
      <PlayersPlace />
      <ServerControls
        buttons={ARK_BUTTONS.map((label) => (
          <ButtonPlace key={label} label={label} size="default" />
        ))}
      >
        <div />
      </ServerControls>
    </div>
  );
}
