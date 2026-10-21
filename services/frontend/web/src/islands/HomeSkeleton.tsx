import { Skeleton } from '@/components/ui/skeleton';

/** The logged-in start page's shape while it loads: the hero's own lines with a pulse for the
 * picture and the name, so nothing moves when it arrives; the cards' places below. */
export function HomeSkeleton() {
  return (
    <div className="flex flex-col gap-4" role="status" aria-label="Loading" aria-busy="true">
      <section className="flex flex-col items-center gap-2 pt-12 pb-12 text-center md:pt-20 lg:pt-24">
        <Skeleton className="mb-2 h-6 w-32 rounded-4xl" />
        <div className={HERO_NAME}>
          <Skeleton className="size-12 shrink-0 rounded-full md:size-14" />
          {/* A line of the name's height (the line box), with the pulse inside it. */}
          <span>
            <Skeleton className="inline-block h-[0.8em] w-[5em] rounded-2xl align-middle" />
          </span>
        </div>
      </section>
      <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
        <Skeleton className="h-44 rounded-2xl" />
        <Skeleton className="h-44 rounded-2xl" />
      </div>
    </div>
  );
}

/** The hero's line with the picture and the name (HomeCards), and its place here. */
export const HERO_NAME =
  'flex max-w-full items-center justify-center gap-4 text-4xl leading-tight font-semibold tracking-tight md:text-5xl lg:leading-[1.1]';
