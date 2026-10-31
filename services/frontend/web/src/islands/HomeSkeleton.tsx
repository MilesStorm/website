import { CARD_GRID, FeatureCardPlaceholder } from '@/components/FeatureCard';
import { FEATURES } from '@/lib/features';
import { Storm } from './Storm';
import { Skeleton } from '@/components/ui/skeleton';

/** The logged-in start page's shape while it loads: the hero's own lines with a pulse for the
 * picture and the name, so nothing moves when it arrives; the viewer's cards below, in pulses
 * (FeatureCardPlaceholder). */
export function HomeSkeleton() {
  return (
    <div className="flex flex-col gap-4" role="status" aria-label="Loading" aria-busy="true">
      <section className={HERO_SECTION}>
        <Storm />
        <Skeleton className="mb-2 h-6 w-32 rounded-4xl" />
        <div className={HERO_NAME}>
          <Skeleton className="size-12 shrink-0 rounded-full md:size-14 3xl:size-16" />
          {/* A line of the name's height (the line box), with the pulse inside it. */}
          <div>
            <span className="inline-block h-[0.8em] w-[5em] animate-pulse rounded-2xl bg-muted align-middle" />
          </div>
        </div>
      </section>
      {/* The cards homeCards gives this viewer, in its order: each feature open where the viewer
          has its permission, then locked where they lack it (and it's advertised). The server's
          attributes on <html> pick them before any script runs (lib/gates.ts). */}
      <div className={CARD_GRID}>
        {FEATURES.map((feature) => (
          <div key={feature.to} className="contents" data-requires={feature.permission ?? undefined}>
            <FeatureCardPlaceholder feature={feature} state="open" />
          </div>
        ))}
        {FEATURES.filter((f) => f.permission !== null && f.advertised).map((feature) => (
          <div key={`${feature.to}-locked`} className="contents" data-lacks={feature.permission!}>
            <FeatureCardPlaceholder feature={feature} state="locked" />
          </div>
        ))}
      </div>
    </div>
  );
}

/** The hero (HomeCards, and the logged-out page's), and its place here: its spacing leaves the storm room to fade out. From 1920 more of it, round a larger name:
 * the page's one display element keeps its weight over a wider row of cards. */
export const HERO_SECTION =
  'relative flex flex-col items-center gap-2 pt-12 pb-16 text-center md:pt-20 md:pb-20 lg:pt-24 3xl:pt-32 3xl:pb-24';

/** The hero's line with the picture and the name, and its place here. */
export const HERO_NAME =
  'flex max-w-full items-center justify-center gap-4 text-4xl leading-tight font-semibold tracking-tight md:text-5xl lg:leading-[1.1] 3xl:text-6xl';
