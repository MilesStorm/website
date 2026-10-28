import { useEffect, useRef } from 'react';
import { island } from '@/lib/island';
import { maskImage, startStorm } from '@/lib/storm';
import '@/styles/storm.css';

/** Where the glow sits: behind a hero's heading block (`hero`), or behind a greeting and a name
 * (`name`, a much smaller block, so a smaller light). */
const GLOW = {
  hero: 'h-64 w-[40rem]',
  name: 'h-44 w-[28rem]',
};

/** The storm behind a hero: a grid of dots across the viewport and a soft glow behind the hero's
 * words, in the `relative` box it sits in (the hero section). The field runs edge to edge (half
 * as wide again as a phone's screen, so its sides fall off past the screen's edges, not inside
 * them; the page wrapper clips the overflow with `overflow-x-clip`, so there is no scrollbar),
 * from 8rem above the section, behind the header, to the section's foot, so the dots are gone
 * before the cards under the hero. Brightest round the words and fading out before its own edges
 * (the mask, lib/storm.ts). Without motion (the profile's choice, else the system's), and until
 * the script has run, the CSS dots stay as a still frame; with it, a canvas takes over (rain,
 * strikes, the pointer parting the dots). */
export const Storm = island('Storm', function Storm({ glow = 'hero' }: { glow?: keyof typeof GLOW }) {
  const root = useRef<HTMLDivElement>(null);
  useEffect(() => startStorm(root.current!), []);
  return (
    <div
      ref={root}
      aria-hidden="true"
      className="storm pointer-events-none absolute -top-32 bottom-0 left-1/2 -z-10 w-[150vw] -translate-x-1/2 overflow-hidden md:w-screen"
    >
      {/* On the section's middle: 4rem below the field's (the field reaches 8rem above it). */}
      <div
        className={`storm-glow absolute top-[calc(50%+4rem)] left-1/2 max-w-[92vw] -translate-x-1/2 -translate-y-1/2 ${GLOW[glow]}`}
      />
      <div className="storm-flash absolute inset-0 opacity-0" />
      {/* The mask is the canvas's too (lib/storm.ts), so it's set from there. */}
      <div className="storm-dots absolute inset-0" style={{ maskImage, WebkitMaskImage: maskImage }} />
      <canvas className="storm-canvas absolute inset-0 hidden size-full" />
    </div>
  );
});
