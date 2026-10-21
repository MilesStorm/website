// The one client for the Rust server's JSON API, typed by schema.d.ts (generated from the
// server's OpenAPI document). Every URL that points at the server goes through `bffUrl`, so a
// bundled app (Tauri) can point them elsewhere by changing `BASE` alone.
import createClient, { type Middleware } from 'openapi-fetch';
import type { components, paths } from './schema';
import { isPagePath } from '@/lib/pages';

export type Schemas = components['schemas'];

/** Where the server is: the page's own origin. */
const BASE = '';

/** `path` on the server: API calls, pictures, OAuth links, the login redirect, WebSockets. */
export function bffUrl(path: string): string {
  return BASE + path;
}

/** A WebSocket address on the server: `path` from `bffUrl`, with `ws:`/`wss:` for the page's
 * `http:`/`https:`. */
export function wsUrl(path: string, page: Pick<Location, 'href'> = location): string {
  const url = new URL(bffUrl(path), page.href);
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
  return url.href;
}

/** A `logged_out` answer sends the user to log in, and back here afterwards when this is a page
 * the server knows. Any other error is the caller's to show. */
const loggedOut: Middleware = {
  async onResponse({ response }) {
    if (response.status !== 401) return undefined;
    const body = (await response.clone().json().catch(() => null)) as Schemas['ApiError'] | null;
    if (body?.error === 'logged_out') {
      const here = location.pathname;
      location.assign(bffUrl(isPagePath(here) ? `/login?next=${encodeURIComponent(here)}` : '/login'));
    }
    return undefined;
  },
};

/** Builds a client. `fetch` is looked up on the global object at each call, never captured:
 * the tracing script wraps it after this module may have loaded (TRACING.md, "Browser"). */
export function makeClient(fetch: (request: Request) => Promise<Response> = (request) => globalThis.fetch(request)) {
  // Absolute: a Request can't be built from a relative URL outside a document.
  const client = createClient<paths>({ baseUrl: BASE || location.origin, fetch });
  client.use(loggedOut);
  return client;
}

export const client = makeClient();
