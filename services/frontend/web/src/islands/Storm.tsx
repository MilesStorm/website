import { useEffect, useRef } from 'react';
import { island } from '@/lib/island';
import { maskImage, startStorm } from '@/lib/storm';
import '@/styles/storm.css';

/** The storm behind a hero: a soft glow and a grid of dots, centred on the `relative` box it sits in
 * (a hero, or one around it) and as wide as the viewport, so it runs edge to edge whatever the
 * container around the hero (the page wrapper clips the sideways overflow with `overflow-x-clip`,
 * so there is no scrollbar). 20rem taller than the box (28rem from `md`), so a short hero gets the
 * same storm; it reaches past the section, and the hero's spacing keeps it clear of what follows.
 * Full strength out to a share of its radii, then fading out before its own edges. Without motion
 * (the profile's choice, else the system's), and until the script has run, the CSS dots stay as a
 * still frame; with it, a canvas takes over (rain, strikes, the pointer parting the dots). */
export const Storm = island('Storm', function Storm() {
  const root = useRef<HTMLDivElement>(null);
  useEffect(() => startStorm(root.current!), []);
  return (
    <div
      ref={root}
      aria-hidden="true"
      className="storm pointer-events-none absolute top-1/2 left-1/2 -z-10 h-[calc(100%+20rem)] w-screen -translate-x-1/2 -translate-y-1/2 overflow-hidden md:h-[calc(100%+28rem)]"
    >
      <div className="storm-glow absolute inset-0" />
      <div className="storm-flash absolute inset-0 opacity-0" />
      {/* The mask is the canvas's too (lib/storm.ts), so it's set from there. */}
      <div className="storm-dots absolute inset-0" style={{ maskImage, WebkitMaskImage: maskImage }} />
      <canvas className="storm-canvas absolute inset-0 hidden size-full" />
    </div>
  );
});
