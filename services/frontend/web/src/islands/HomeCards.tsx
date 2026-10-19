import { useEffect, useState } from 'react';
import { readBootstrap, type Viewer } from '@/api/session';
import { FeatureCard } from '@/components/FeatureCard';
import { homeCards } from '@/lib/features';
import { bffUrl } from '@/api/client';
import defaultPicture from '@/assets/default-profile.png';
import { HomeSkeleton } from './HomeSkeleton';

/** The logged-in start page: a greeting and the parts of the site, open or locked. */
export function HomeCards() {
  const [viewer, setViewer] = useState<Viewer | null>(null);
  useEffect(() => {
    void readBootstrap().then((b) => setViewer(b.viewer));
  }, []);
  if (!viewer) return <HomeSkeleton />;
  return (
    <div className="flex flex-col gap-10">
      <section className="relative overflow-hidden rounded-xl border bg-card px-6 py-10 shadow-xs sm:px-10">
        <div className="relative flex items-center gap-5">
          <img
            src={viewer.picture_url ? bffUrl(viewer.picture_url) : defaultPicture.src}
            alt=""
            width={64}
            height={64}
            className="size-16 shrink-0 rounded-full ring-2 ring-ring ring-offset-2 ring-offset-card"
          />
          <div>
            <p className="text-sm text-muted-foreground">Welcome back</p>
            <h1 className="text-3xl font-extrabold">{viewer.shown_name}</h1>
          </div>
        </div>
      </section>
      <section className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
        {homeCards(viewer.permissions).map(({ feature, state }) => (
          <FeatureCard key={feature.title} feature={feature} state={state} />
        ))}
      </section>
    </div>
  );
}
