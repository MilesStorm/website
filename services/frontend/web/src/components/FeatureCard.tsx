import { ArrowRight } from 'lucide-react';
import { buttonVariants } from '@/components/ui/button';
import { Card, CardAction, CardDescription, CardFooter, CardHeader, CardTitle } from '@/components/ui/card';
import type { CardState, Feature } from '@/lib/features';
import { cn } from '@/lib/utils';

const NOTE: Record<Exclude<CardState, 'open'>, string> = {
  locked: 'Ask the site owner for access.',
  preview: 'For invited accounts.',
};

/** A part of the site, like the demo cards on ui.shadcn.com's front page: a link when it's open
 * to the viewer (over the whole card), a description otherwise. */
export function FeatureCard({ feature, state }: { feature: Feature; state: CardState }) {
  const open = state === 'open';
  // From the link, not useId: Astro renders each card on its own, so useId's would repeat. Only
  // open cards have it: the logged-out page's previews (hidden, but there) would repeat it too.
  const id = open ? `feature${feature.to.replace('/', '-')}` : undefined;
  return (
    <Card
      className={cn(
        'relative h-full',
        open &&
          'transition-shadow hover:ring-foreground/20 has-[a:focus-visible]:ring-3 has-[a:focus-visible]:ring-ring/50',
        state === 'locked' && 'border border-dashed ring-0',
      )}
    >
      <CardHeader>
        <CardTitle>
          <h2 id={id && `${id}-title`}>{feature.title}</h2>
        </CardTitle>
        <CardDescription id={id && `${id}-text`}>{feature.text}</CardDescription>
        <CardAction>
          <span className="grid size-9 place-items-center rounded-full bg-muted text-foreground">
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
      <CardFooter className="mt-auto">
        {open ? (
          // Only looks like the link's button: the link (below) covers the card.
          <span
            aria-hidden="true"
            className={cn(
              buttonVariants({ variant: 'secondary', size: 'sm' }),
              'w-full group-hover/card:bg-[color-mix(in_oklch,var(--secondary),var(--foreground)_5%)]',
            )}
          >
            Open
            <ArrowRight />
          </span>
        ) : (
          <p className="text-xs text-muted-foreground">{NOTE[state]}</p>
        )}
      </CardFooter>
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
