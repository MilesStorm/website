import { useEffect, useRef } from 'react';
import { island } from '@/lib/island';
import { maskImage, startStorm } from '@/lib/storm';
import '@/styles/storm.css';

/** The storm behind a hero: a grid of dots across the viewport, in the `relative` box it sits in (the hero section). The field runs edge to edge (the
 * page wrapper clips the overflow with `overflow-x-clip`, so there is no scrollbar; on a phone
 * its mask is wider than the screen, so its sides fall off past the edges, not inside them),
 * from 8rem above the section, behind the header, to the section's foot, so the dots are gone
 * before the cards under the hero. Brightest round the words and fading out before its own edges
 * (the mask, lib/storm.ts). Without motion (the profile's choice, else the system's), and until
 * the script has run, the CSS dots stay as a still frame; with it, a canvas takes over (rain,
 * strikes, the pointer parting the dots). */
export const Storm = island('Storm', function Storm() {
  const root = useRef<HTMLDivElement>(null);
  useEffect(() => startStorm(root.current!), []);
  return (
    <div
      ref={root}
      aria-hidden="true"
      className="storm pointer-events-none absolute -top-32 bottom-0 left-1/2 -z-10 w-screen -translate-x-1/2 overflow-hidden"
    >
      <div className="storm-flash absolute inset-0 opacity-0" />
      {/* The mask is the canvas's too (lib/storm.ts), so it's set from there. */}
      <div className="storm-dots absolute inset-0" style={{ maskImage, WebkitMaskImage: maskImage }} />
      <canvas className="storm-canvas absolute inset-0 hidden size-full" />
    </div>
  );
});
