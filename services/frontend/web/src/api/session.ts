// Who is looking at the page. The server puts it into every page it serves (a `bootstrap` JSON
// tag and `data-session`/`data-perms` on <html>); a page served without it (`astro dev`, a
// bundled app) asks `GET /api/session` instead. Both are the same typed `Bootstrap`.
import { bffUrl, client, type Schemas } from './client';

export type Bootstrap = Schemas['Bootstrap'];
export type Viewer = Schemas['Viewer'];
export type KnownPermission = Schemas['KnownPermission'];

let loaded: Promise<Bootstrap> | undefined;

/** This page's viewer, read once per page. */
export function readBootstrap(): Promise<Bootstrap> {
  loaded ??= read();
  return loaded;
}

async function read(): Promise<Bootstrap> {
  const tag = document.getElementById('bootstrap');
  if (tag?.textContent) return JSON.parse(tag.textContent) as Bootstrap;
  return fetchBootstrap();
}

/** Asks the server, ignoring what the page says. A failure counts as logged out. */
export async function fetchBootstrap(): Promise<Bootstrap> {
  const { data } = await client.GET('/api/session');
  return data ?? { viewer: null };
}

/** What the server writes on <html> for `bootstrap`: the CSS gates read it. */
export function applySession(bootstrap: Bootstrap, root: HTMLElement = document.documentElement): void {
  root.dataset.session = bootstrap.viewer ? 'in' : 'out';
  root.dataset.perms = bootstrap.viewer?.permissions.join(' ') ?? '';
}

/** Whether two answers show the same person with the same permissions. */
export function sameViewer(a: Bootstrap, b: Bootstrap): boolean {
  const key = (x: Bootstrap) => (x.viewer ? `${x.viewer.username}:${x.viewer.permissions.join(' ')}` : '');
  return key(a) === key(b);
}

/** Logs out, then loads the start page: a new page, so everything on it is the logged-out one.
 * The user menu's and the profile page's. */
export async function logOut(): Promise<void> {
  await client.DELETE('/api/session');
  location.assign(bffUrl('/'));
}

const VIEWER_CHANGED = 'ms:viewer';

/** Tells this page's other islands (the user menu) that the viewer's name or picture changed. */
export function viewerChanged(viewer: Viewer): void {
  window.dispatchEvent(new CustomEvent<Viewer>(VIEWER_CHANGED, { detail: viewer }));
}

/** Calls `then` on every change [`viewerChanged`] announces; the result stops listening. */
export function onViewerChange(then: (viewer: Viewer) => void): () => void {
  const listener = (e: Event) => then((e as CustomEvent<Viewer>).detail);
  window.addEventListener(VIEWER_CHANGED, listener);
  return () => window.removeEventListener(VIEWER_CHANGED, listener);
}
