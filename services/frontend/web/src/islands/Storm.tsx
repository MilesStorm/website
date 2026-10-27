import { useEffect, useRef } from 'react';
import { island } from '@/lib/island';
import { startStorm } from '@/lib/storm';
import '@/styles/storm.css';

/** The storm behind a hero: a soft glow and a grid of dots, centred on the `relative` box it sits in
 * (a hero, or one around it) and as wide as it, at least 20rem tall so a short hero gets the same storm
 * (it reaches past the section, and the hero's spacing keeps it clear of what follows). It fades
 * out before its own edges. Without motion (the profile's choice, else the system's), and until
 * the script has run, the CSS dots stay as a still frame; with it, a canvas takes over (rain,
 * strikes, the pointer parting the dots). */
export const Storm = island('Storm', function Storm() {
  const root = useRef<HTMLDivElement>(null);
  useEffect(() => startStorm(root.current!), []);
  return (
    <div
      ref={root}
      aria-hidden="true"
      className="storm pointer-events-none absolute inset-x-0 top-1/2 -z-10 h-[max(100%,20rem)] -translate-y-1/2 overflow-hidden"
    >
      <div className="storm-glow absolute inset-0" />
      <div className="storm-flash absolute inset-0 opacity-0" />
      <div className="storm-dots absolute inset-0" />
      <canvas className="storm-canvas absolute inset-0 hidden size-full" />
    </div>
  );
});
