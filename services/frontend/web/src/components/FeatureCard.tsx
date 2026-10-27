import { Card, CardAction, CardDescription, CardFooter, CardHeader, CardTitle } from '@/components/ui/card';
import type { CardState, Feature } from '@/lib/features';
import { cn } from '@/lib/utils';

const NOTE: Record<Exclude<CardState, 'open'>, string> = {
  locked: 'Ask the site owner for access.',
  preview: 'For invited accounts.',
};

/** The start page's cards: a column on a phone, two from `sm`, three from `lg`. A card is as tall
 * as its text, and the cards of a row are as tall as the tallest. */
export const CARD_GRID = 'grid gap-4 sm:grid-cols-2 lg:grid-cols-3';

/** The logged-out page's two previews: two cards of at most 28rem each, in the middle under the
 * hero (not stretched across the whole container on a wide screen). */
export const PREVIEW_GRID = 'mx-auto grid w-full max-w-4xl gap-4 sm:grid-cols-2';

/** What a card's icon sits in. */
const ICON_BADGE = 'grid size-9 place-items-center rounded-full bg-muted text-foreground';

/** A part of the site, like the demo cards on ui.shadcn.com's front page: a link when it's open
 * to the viewer (over the whole card, which answers the pointer and the keyboard), a description
 * otherwise. */
export function FeatureCard({ feature, state }: { feature: Feature; state: CardState }) {
  const open = state === 'open';
  // From the link, not useId: Astro renders each card on its own, so useId's would repeat. Only
  // open cards have it: the logged-out page's previews (hidden, but there) would repeat it too.
  const id = open ? `feature${feature.to.replaceAll('/', '-')}` : undefined;
  return (
    <Card
      className={cn(
        'relative h-full',
        open &&
          'transition-colors hover:bg-muted/50 hover:ring-foreground/20 has-[a:focus-visible]:ring-3 has-[a:focus-visible]:ring-ring/50',
        state === 'locked' && 'border border-dashed ring-0',
      )}
    >
      <CardHeader>
        <CardTitle>
          <h2 id={id && `${id}-title`}>{feature.title}</h2>
        </CardTitle>
        <CardDescription id={id && `${id}-text`}>{feature.text}</CardDescription>
        <CardAction>
          <span
            className={cn(
              ICON_BADGE,
              'transition-colors',
              open &&
                'group-hover/card:bg-primary group-hover/card:text-primary-foreground group-has-[a:focus-visible]/card:bg-primary group-has-[a:focus-visible]/card:text-primary-foreground',
            )}
          >
            <svg
              xmlns="http://www.w3.org/2000/svg"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.8"
              strokeLinecap="round"
              strokeLinejoin="round"
              className="size-4.5"
              aria-hidden="true"
            >
              <path d={feature.icon} />
            </svg>
          </span>
        </CardAction>
      </CardHeader>
      {!open && (
        <CardFooter className="mt-auto">
          <p className="text-xs text-muted-foreground">{NOTE[state]}</p>
        </CardFooter>
      )}
      {id && (
        // Over the whole card, named by its title and described by its text. Not wrapped around
        // the title: the header is a container, so a stretched link would cover only the header.
        <a
          href={feature.to}
          aria-labelledby={`${id}-title`}
          aria-describedby={`${id}-text`}
          className="absolute inset-0 outline-none"
        />
      )}
    </Card>
  );
}

/** A card's place while it loads: the card itself, its words in pulses (the words are there, in the
 * colour of the pulse, so every line wraps as it will and the card has its height at every width),
 * without a link; a locked card's with its border and its footer's line. */
export function FeatureCardPlaceholder({ feature, state }: { feature: Feature; state: CardState }) {
  const pulse = 'animate-pulse rounded-md bg-muted text-transparent select-none box-decoration-clone';
  return (
    <Card className={cn('h-full', state === 'locked' && 'border border-dashed ring-0')} aria-hidden="true">
      <CardHeader>
        <CardTitle>
          <span className={pulse}>{feature.title}</span>
        </CardTitle>
        <CardDescription>
          <span className={pulse}>{feature.text}</span>
        </CardDescription>
        <CardAction>
          <span className={cn(ICON_BADGE, 'animate-pulse')} />
        </CardAction>
      </CardHeader>
      {state !== 'open' && (
        <CardFooter className="mt-auto">
          <p className="text-xs text-muted-foreground">
            <span className={pulse}>{NOTE[state]}</span>
          </p>
        </CardFooter>
      )}
    </Card>
  );
}
