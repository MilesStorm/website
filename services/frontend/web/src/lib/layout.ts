// The site's one horizontal frame and vertical rhythm. Every page's content and the navbar's row
// sit in the same container, so the logo's left edge lines up with page titles and cards at every
// width. Used by components/Navbar.astro and components/Page.astro (and by islands that draw
// their own full-width sections); nothing else spells these classes out.

/** The container: centred, gutters of 16px on a phone, 24px from `sm`, 32px from `lg`, 48px from
 * 1920. At most 80rem (1280px) wide, 96rem from 1536 and 120rem from 1920: about 71% of a 1440p
 * screen. Every page takes all of it, so the left edge never moves between pages. Type never grows
 * with the window: the pages use the room with columns, not bigger controls. */
export const CONTAINER =
  'mx-auto w-full max-w-7xl px-4 sm:px-6 lg:px-8 2xl:max-w-[96rem] 3xl:max-w-[120rem] 3xl:px-12';

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

/** The site's column grid: one column on a phone, two from `sm`, thirds from `lg`. Home's cards,
 * Ark's cards and Enshrouded's figures share it, so their columns line up from page to page. */
export const THIRDS = `grid gap-4 sm:grid-cols-2 lg:grid-cols-3 ${COLUMN_GAP_LG}`;
