import { Skeleton } from '@/components/ui/skeleton';
import { ButtonPlace } from '@/components/Placeholders';
import { SECTIONS, Section, Setting } from '@/components/Setting';
import {
  ACCOUNT_INTRO,
  APPEARANCE_INTRO,
  DANGER_INTRO,
  DELETE_INTRO,
  LOGOUT_INTRO,
  SESSION_INTRO,
  MOTION_INTRO,
  NAME_HINT,
  PICTURE_HINT,
  SHARING_DELETE_INTRO,
  SHARING_DELETE_TITLE,
  SHARING_FACTS,
  SHARING_FACTS_INTRO,
  SHARING_FACTS_TITLE,
  SHARING_INTRO,
  SHARING_SECTION_INTRO,
  SHARING_SWITCH,
  SHARING_TITLE,
  THEME_INTRO,
  USERNAME_HINT,
} from '@/lib/copy';

/** The delete buttons' labels: shorter on a phone, where the rest stays in the accessible name. */
export const SHARING_DELETE_LABEL = (
  <>
    Delete <span className="max-sm:sr-only">my shared</span> pictures
  </>
);
export const DELETE_OPEN_LABEL = (
  <>
    Delete <span className="max-sm:sr-only">my account…</span>
  </>
);

// The profile's place while it loads: its cards with their own text, and a pulse where the
// account's details and each button go, so nothing moves when the profile arrives
// (e2e/profile.spec.ts measures it). It is the usual answer: a confirmed email (or none), and the
// sharing card only for viewers with `arcane` (the CSS gate, from the page's own attributes).

/** The roll-sharing card's place (also while its own setting loads). */
export function SharingSkeleton() {
  return (
    <Section title={SHARING_TITLE} description={SHARING_SECTION_INTRO}>
      <Setting title={SHARING_SWITCH} description={SHARING_INTRO}>
        <span aria-hidden="true" className="inline-flex h-[18.4px] w-[32px] animate-pulse rounded-full bg-muted" />
      </Setting>
      <Setting title={SHARING_FACTS_TITLE} description={SHARING_FACTS_INTRO}>
        <ButtonPlace label="Show" />
      </Setting>
      <Setting title={SHARING_DELETE_TITLE} description={SHARING_DELETE_INTRO}>
        <ButtonPlace label={SHARING_DELETE_LABEL} />
      </Setting>
    </Section>
  );
}

/** What sharing saves, in three tiles: the card's disclosure, loaded or not. */
export function SharingFacts() {
  return (
    <dl className="grid gap-3 sm:grid-cols-3">
      {SHARING_FACTS.map(([term, text]) => (
        <div key={term} className="rounded-xl bg-muted/50 p-4">
          <dt className="text-xs font-medium tracking-wide text-muted-foreground uppercase">{term}</dt>
          <dd className="mt-1 text-sm">{text}</dd>
        </div>
      ))}
    </dl>
  );
}

/** The Appearance section's place: its text, and a pulse as big as each segmented control (a
 * build-time fallback can't know the saved choice). */
function AppearancePlace() {
  return (
    <Section title="Appearance" description={APPEARANCE_INTRO}>
      <Setting wide title="Theme" description={THEME_INTRO}>
        <Skeleton className="h-[34px] w-full rounded-4xl @lg:w-[272px]" />
      </Setting>
      <Setting wide title="Motion" description={MOTION_INTRO}>
        <Skeleton className="h-[34px] w-full rounded-4xl @lg:w-[216px]" />
      </Setting>
    </Section>
  );
}

export function ProfileSkeleton() {
  return (
    <div role="status" aria-label="Loading your account" aria-busy="true" className={SECTIONS}>
      <Section title="Account" description={ACCOUNT_INTRO}>
        <Setting
          title="Profile picture"
          description={PICTURE_HINT}
          lead={<Skeleton className="size-12 shrink-0 rounded-full" />}
        >
          <ButtonPlace label="Change" />
        </Setting>
        <Setting wide title="Display name" description={NAME_HINT}>
          <div className="flex w-full gap-2 @lg:w-auto">
            <Skeleton className="h-9 min-w-0 flex-1 rounded-4xl @lg:w-52 @lg:flex-none" />
            <ButtonPlace label="Save" size="default" />
          </div>
        </Setting>
        <Setting title="Username" description={USERNAME_HINT} wide>
          <Skeleton className="h-5 w-10" />
        </Setting>
        <div className="flex flex-col gap-1">
          <div className="flex items-center gap-2">
            <span className="text-sm font-medium">Email</span>
            <Skeleton className="h-5 w-20" />
          </div>
          <Skeleton className="h-5 w-56 max-w-full" />
        </div>
      </Section>
      <AppearancePlace />
      {/* The usual answer: the sharing store is set up. Without it the section is left out, and if
          its setting can't be read it is a one-line alert (Profile's SharingCard). */}
      <div data-requires="arcane">
        <SharingSkeleton />
      </div>
      <Section title="Session" description={SESSION_INTRO}>
        <Setting title="Log out" description={LOGOUT_INTRO}>
          <ButtonPlace label="Log out" />
        </Setting>
      </Section>
      <Section danger title="Danger zone" description={DANGER_INTRO}>
        <Setting title="Delete account" description={DELETE_INTRO}>
          <ButtonPlace label={DELETE_OPEN_LABEL} />
        </Setting>
      </Section>
    </div>
  );
}
