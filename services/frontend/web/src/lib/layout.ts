// The site's one horizontal frame and vertical rhythm. Every page's content and the navbar's row
// sit in the same container, so the logo's left edge lines up with page titles and cards at every
// width. Used by components/Navbar.astro and components/Page.astro (and by islands that draw
// their own full-width sections); nothing else spells these classes out.

/** The container: centred, gutters of 16px on a phone, 24px from `sm`, 32px from `lg`, 48px from
 * 2560. At most 80rem wide, then 88rem from 1920, 100rem from 2560 and 120rem from 3200: with the
 * root size growing on wide screens (global.css), about 85% of the window there, not a strip. */
export const CONTAINER =
  'mx-auto w-full max-w-7xl px-4 sm:px-6 lg:px-8 3xl:max-w-[88rem] 4xl:max-w-[100rem] 4xl:px-12 5xl:max-w-[120rem]';

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
