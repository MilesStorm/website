// A dropdown menu that a tap opens on iOS. Radix's trigger opens on `pointerdown`, which React
// listens for on the island's root, Astro's <astro-island>; it is `display: contents`, and iOS
// Safari doesn't send a page touch pointer events when the only listener is on such an element
// (facebook/react#29890, radix-ui/primitives#2580). The click still arrives. So a mouse opens
// the menu on `pointerdown` as before, and anything else (a finger, a pen, a screen reader) on
// `click`, as Radix's Select does; a scroll that starts on the trigger no longer opens it either.
import { useRef, useState, type PointerEvent } from 'react';

/** Props for `DropdownMenu` (`root`) and `DropdownMenuTrigger` (`trigger`). */
export function useMenu() {
  const [open, setOpen] = useState(false);
  // The last pointerdown was a mouse press Radix has already acted on. Cleared when the menu
  // closes too: Radix's modal menu takes pointer events off the page, so that press's click
  // may never reach the trigger, and a stale flag would swallow the next tap.
  const mouse = useRef(false);
  return {
    root: {
      open,
      onOpenChange: (next: boolean) => {
        if (!next) mouse.current = false;
        setOpen(next);
      },
    },
    trigger: {
      onPointerDown: (e: PointerEvent) => {
        if (e.pointerType !== 'mouse') {
          mouse.current = false;
          // Radix skips its own handler for a prevented event.
          e.preventDefault();
          return;
        }
        // Radix ignores other buttons and Ctrl+click, and so does the click after them.
        mouse.current = e.button === 0 && !e.ctrlKey;
      },
      onClick: () => {
        if (mouse.current) mouse.current = false;
        else setOpen((was) => !was);
      },
    },
  };
}
