// The site's one horizontal frame and vertical rhythm. Every page's content and the navbar's row
// sit in the same container, so the logo's left edge lines up with page titles and cards at every
// width. Used by components/Navbar.astro and components/Page.astro (and by islands that draw
// their own full-width sections); nothing else spells these classes out.

/** The container: centred, gutters of 16px on a phone, 24px from `sm`, 32px from `lg`. At most
 * 80rem (1280px) wide below 1920; from 1920 as wide as the window (up to 200rem), with gutters that
 * grow with it (4.5% of the width, 2rem to 8rem): about 91% of a 1440p screen. Type never grows
 * with the window: the pages use the room with columns and panels side by side. */
export const CONTAINER =
  'mx-auto w-full max-w-7xl px-4 sm:px-6 lg:px-8 3xl:max-w-[200rem] 3xl:px-[clamp(2rem,4.5vw,8rem)]';

/** A page's width from 1920 (Page.astro's `measure`), centred: what its content wants, not the
 * window's. Below 1920 every page has the container's. Arcane has none: its camera takes the room. */
export const MEASURE = {
  ark: '3xl:max-w-[56rem]',
  profile: '3xl:max-w-[80rem]',
  admin: '3xl:max-w-[80rem]',
  enshrouded: '3xl:max-w-[90rem]',
};

/** A page's vertical rhythm: the space above and below its content. */
export const PAGE_PADDING = 'py-8 sm:py-10 lg:py-12';

/** The gap between columns from `lg`, the same as between sections: a row of tiles lines up with
 * the grid of cards under it. */
export const COLUMN_GAP_LG = 'lg:gap-8';

/** The gap between a page's sections (its header, then each block of content). */
export const PAGE_GAP = `gap-6 ${COLUMN_GAP_LG}`;

/** A page's content column: what Page.astro puts in <main>, and what an island that fills the
 * page returns as its outermost element so its sections keep the same gap. */
export const PAGE_STACK = `flex flex-col ${PAGE_GAP}`;
