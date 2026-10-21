import { Card, CardAction, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from '@/components/ui/card';
import { Field, FieldDescription, FieldLabel } from '@/components/ui/field';
import { Skeleton } from '@/components/ui/skeleton';
import {
  ACCOUNT_INTRO,
  DELETE_INTRO,
  DELETE_OPEN,
  NAME_HINT,
  PICTURE_HINT,
  SHARING_DELETE,
  SHARING_FACTS,
  SHARING_INTRO,
  SHARING_NOTE,
  SHARING_TITLE,
} from '@/lib/copy';
import { cn } from '@/lib/utils';

// The profile's place while it loads: its cards with their own text, and a pulse where the
// account's details and each button go, so nothing moves when the profile arrives
// (e2e/profile.spec.ts measures it). It is the usual answer: a confirmed email (or none), and the
// sharing card only for viewers with `arcane` (the CSS gate, from the page's own attributes).

/** A button's place: a pulse as wide as its label (`sm`: h-8, `default`: h-9). */
function ButtonPlace({ label, size = 'sm' }: { label: string; size?: 'sm' | 'default' }) {
  return (
    <span
      aria-hidden="true"
      className={cn(
        'inline-flex shrink-0 animate-pulse items-center rounded-4xl border border-transparent bg-muted px-3 text-sm font-medium whitespace-nowrap text-transparent select-none',
        size === 'sm' ? 'h-8' : 'h-9',
      )}
    >
      {label}
    </span>
  );
}

/** The roll-sharing card's place (also while its own setting loads). */
export function SharingSkeleton() {
  return (
    <Card>
      <CardHeader>
        <CardTitle>
          <h2>{SHARING_TITLE}</h2>
        </CardTitle>
        <CardDescription>{SHARING_INTRO}</CardDescription>
        <CardAction>
          <span aria-hidden="true" className="inline-flex h-[18.4px] w-[32px] animate-pulse rounded-full bg-muted" />
        </CardAction>
      </CardHeader>
      <CardContent>
        <SharingFacts />
      </CardContent>
      <CardFooter className="flex flex-col items-stretch gap-3">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <p className="max-w-lg text-xs text-muted-foreground">{SHARING_NOTE}</p>
          <ButtonPlace label={SHARING_DELETE} />
        </div>
      </CardFooter>
    </Card>
  );
}

/** What sharing saves, in three tiles: the card's body, loaded or not. */
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

export function ProfileSkeleton() {
  return (
    <div role="status" aria-label="Loading your account" aria-busy="true" className="flex flex-col gap-6">
      <Card>
        <CardHeader>
          <CardTitle>
            <h2>Account</h2>
          </CardTitle>
          <CardDescription>{ACCOUNT_INTRO}</CardDescription>
        </CardHeader>
        <CardContent className="grid gap-8 sm:grid-cols-[auto_1fr]">
          <div className="flex flex-col items-center gap-3 sm:items-start">
            <Skeleton className="size-24 rounded-full" />
            <div className="flex gap-2">
              <ButtonPlace label="Upload" />
            </div>
            <p className="text-xs text-muted-foreground">{PICTURE_HINT}</p>
          </div>
          <div className="flex flex-col gap-3">
            <Field>
              <FieldLabel>Display name</FieldLabel>
              <div className="flex flex-wrap gap-2">
                <Skeleton className="h-9 w-full max-w-xs rounded-4xl" />
                <ButtonPlace label="Save" size="default" />
              </div>
              <FieldDescription>
                {NAME_HINT.before}{' '}
                <span className="animate-pulse rounded-md bg-muted font-semibold text-transparent">username</span>
                {NAME_HINT.after}
              </FieldDescription>
            </Field>
          </div>
        </CardContent>
      </Card>
      <Card>
        <CardHeader>
          <CardTitle>
            <h2>Email</h2>
          </CardTitle>
          <CardDescription>
            <Skeleton className="h-5 w-56" />
          </CardDescription>
        </CardHeader>
      </Card>
      {/* The usual answer: the sharing store is set up. Without it the card is left out, and if its
          setting can't be read it is a one-line alert (Profile's SharingCard). */}
      <div data-requires="arcane">
        <SharingSkeleton />
      </div>
      <Card>
        <CardHeader>
          <CardTitle>
            <h2>Delete account</h2>
          </CardTitle>
          <CardDescription>{DELETE_INTRO}</CardDescription>
          <CardAction>
            <ButtonPlace label={DELETE_OPEN} />
          </CardAction>
        </CardHeader>
      </Card>
    </div>
  );
}
