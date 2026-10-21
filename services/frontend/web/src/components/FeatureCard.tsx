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
 * to the viewer, a description otherwise. */
export function FeatureCard({ feature, state }: { feature: Feature; state: CardState }) {
  const body = (
    <>
      <CardHeader>
        <CardTitle>
          <h2>{feature.title}</h2>
        </CardTitle>
        <CardDescription>{feature.text}</CardDescription>
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
        {state === 'open' ? (
          // The whole card is the link: this only looks like its button.
          <span
            className={cn(
              buttonVariants({ variant: 'secondary', size: 'sm' }),
              'w-full group-hover:bg-[color-mix(in_oklch,var(--secondary),var(--foreground)_5%)]',
            )}
          >
            Open
            <ArrowRight aria-hidden="true" />
          </span>
        ) : (
          <p className="text-xs text-muted-foreground">{NOTE[state]}</p>
        )}
      </CardFooter>
    </>
  );
  if (state === 'open') {
    return (
      <a href={feature.to} className="group rounded-2xl outline-none focus-visible:ring-3 focus-visible:ring-ring/50">
        <Card className="h-full transition-shadow group-hover:ring-foreground/20">{body}</Card>
      </a>
    );
  }
  return <Card className={cn('h-full', state === 'locked' && 'border border-dashed ring-0')}>{body}</Card>;
}
