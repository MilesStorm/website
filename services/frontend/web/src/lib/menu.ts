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
  // The last pointerdown was a mouse's, which Radix has already acted on.
  const mouse = useRef(false);
  return {
    root: { open, onOpenChange: setOpen },
    trigger: {
      onPointerDown: (e: PointerEvent) => {
        mouse.current = e.pointerType === 'mouse';
        // Radix skips its own handler for a prevented event.
        if (!mouse.current) e.preventDefault();
      },
      onClick: () => {
        if (mouse.current) mouse.current = false;
        else setOpen((was) => !was);
      },
    },
  };
}
