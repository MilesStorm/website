import { Card, CardContent } from '@/components/ui/card';
import type { CardState, Feature } from '@/lib/features';
import { cn } from '@/lib/utils';

const NOTE: Record<CardState, string> = {
  open: 'Open →',
  locked: 'Ask the site owner for access.',
  preview: 'For invited accounts.',
};

/** A part of the site: a link when it's open to the viewer, a description otherwise. */
export function FeatureCard({ feature, state }: { feature: Feature; state: CardState }) {
  const body = (
    <CardContent className="flex items-start gap-4">
      <div className="grid size-10 shrink-0 place-items-center rounded-lg border bg-muted">
        <svg
          xmlns="http://www.w3.org/2000/svg"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.8"
          strokeLinecap="round"
          strokeLinejoin="round"
          className="size-6"
          aria-hidden="true"
        >
          <path d={feature.icon} />
        </svg>
      </div>
      <div className="min-w-0">
        <h2 className="font-semibold tracking-tight">{feature.title}</h2>
        <p className="mt-1 text-sm text-muted-foreground">{feature.text}</p>
        <p className={cn('mt-3', state === 'open' ? 'text-sm font-medium' : 'text-xs text-muted-foreground')}>
          {NOTE[state]}
        </p>
      </div>
    </CardContent>
  );
  if (state === 'open') {
    return (
      <a href={feature.to} className="group rounded-xl outline-none focus-visible:ring-3 focus-visible:ring-ring/50">
        <Card className="h-full transition group-hover:-translate-y-0.5 group-hover:shadow-md">{body}</Card>
      </a>
    );
  }
  return <Card className={cn('h-full', state === 'locked' && 'border border-dashed shadow-none ring-0')}>{body}</Card>;
}
