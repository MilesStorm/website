import { useEffect, useState } from 'react';
import { readBootstrap, type Viewer } from '@/api/session';
import { CARD_GRID, FeatureCard } from '@/components/FeatureCard';
import { Storm } from './Storm';
import { badgeVariants } from '@/components/ui/badge';
import { homeCards } from '@/lib/features';
import { cn } from '@/lib/utils';
import { bffUrl } from '@/api/client';
import defaultPicture from '@/assets/default-profile.png';
import { HERO_NAME, HERO_SECTION, HomeSkeleton } from './HomeSkeleton';
import { island } from '@/lib/island';

/** The logged-in start page: a greeting and the parts of the site, open or locked. */
export const HomeCards = island('HomeCards', function HomeCards() {
  const [viewer, setViewer] = useState<Viewer | null>(null);
  useEffect(() => {
    void readBootstrap().then((b) => setViewer(b.viewer));
  }, []);
  if (!viewer) return <HomeSkeleton />;
  return (
    <div className="flex flex-col gap-4">
      <section className={HERO_SECTION}>
        <Storm />
        <p className={cn(badgeVariants({ variant: 'secondary' }), 'mb-2 h-6 px-2.5')}>
          <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
            <path d="M13 2 4 14h7l-1 8 9-12h-7z" />
          </svg>
          Welcome back
        </p>
        <div className={HERO_NAME}>
          <img
            src={viewer.picture_url ? bffUrl(viewer.picture_url) : defaultPicture.src}
            alt=""
            width={56}
            height={56}
            className="size-12 shrink-0 rounded-full object-cover ring-2 ring-border md:size-14"
          />
          {/* One line, beside the picture: a name too long for it ends in an ellipsis (the whole of it
              in the tooltip) instead of breaking mid-word away from the picture. */}
          <h1 className="min-w-0 truncate text-primary xl:tracking-tighter" title={viewer.shown_name}>
            {viewer.shown_name}
          </h1>
        </div>
      </section>
      <section className={CARD_GRID}>
        {homeCards(viewer.permissions).map(({ feature, state }) => (
          <FeatureCard key={feature.title} feature={feature} state={state} />
        ))}
      </section>
    </div>
  );
});
