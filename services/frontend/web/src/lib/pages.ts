// The site's pages, as the server's PAGES table lists them (crates/api-types). `PagePath` is
// generated from that table; PAGE_PATHS is its runtime copy, checked against the OpenAPI
// document's `x-pages` by a test.
import type { components } from '@/api/schema';

export type PagePath = components['schemas']['PagePath'];

export const PAGE_PATHS = [
  '/',
  '/asshole',
  '/account-deleted',
  '/login',
  '/register',
  '/forgot-password',
  '/reset-password',
  '/verify-email',
  '/delete-account',
  '/invite',
  '/profile',
  '/ark',
  '/enshrouded',
  '/admin',
] as const satisfies readonly PagePath[];

// Fails to compile when a page is missing from PAGE_PATHS.
type Missing = Exclude<PagePath, (typeof PAGE_PATHS)[number]>;
const complete: [Missing] extends [never] ? true : Missing = true;
void complete;

/** Pages the Dioxus app still serves. Each moves into `PagePath` with the PR that rebuilds it. */
export type DioxusPage = '/arcane';

/** Where a link on the site may go. */
export type Href = PagePath | DioxusPage;

export function isPagePath(path: string): path is PagePath {
  return (PAGE_PATHS as readonly string[]).includes(path);
}
