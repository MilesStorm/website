// Landing after a login: the page the login page was opened for (`?next=`, only when it is one
// of the site's pages), unless the server says an invite waits. Every login path ends here,
// in a full page load, so everything on the next page is the logged-in one.
import { bffUrl, client, type Schemas } from '@/api/client';
import { isPagePath, type PagePath } from './pages';

/** `?next=` of `search`, when it is a page of the site; anything else is dropped. */
export function nextFrom(search: string): PagePath | null {
  const next = new URLSearchParams(search).get('next');
  return next !== null && isPagePath(next) ? next : null;
}

/** The address of a `NextPage`. */
export function href(page: Schemas['NextPage']): string {
  return page.fragment === null ? page.path : `${page.path}#${page.fragment}`;
}

/** Asks the server where to go now and goes there. If it can't answer, `next` (or `/`). */
export async function landAfterLogin(next: PagePath | null): Promise<void> {
  const { data } = await client.POST('/api/session/after-login', { body: { next } });
  location.assign(bffUrl(data ? href(data) : (next ?? '/')));
}

/** Where an OAuth button starts, carrying `next` to the server. */
export function oauthStart(provider: 'github' | 'google', next: PagePath | null): string {
  const path = `/oauth/start/${provider}`;
  return bffUrl(next ? `${path}?next=${encodeURIComponent(next)}` : path);
}

/** A link whose code was cut off (the fragment is empty). */
export const MISSING_CODE = 'This link is incomplete. Open it straight from the email, or copy the whole link.';

/** The code an emailed link carries in its fragment (`#<code>`): it never reaches the server in
 * a URL, only in a request body. Empty when the link was cut short. */
export function linkCode(hash: string = location.hash): string {
  return hash.replace(/^#/, '').trim();
}
