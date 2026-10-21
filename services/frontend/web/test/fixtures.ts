// API answers typed by the generated schema: a field renamed in Rust fails the type check here.
import type { Bootstrap } from '@/api/session';

export const loggedOut: Bootstrap = { viewer: null };

export const ann: Bootstrap = {
  viewer: {
    username: 'ann',
    shown_name: 'Ann </script> Example',
    picture_url: null,
    permissions: ['arcane', 'llama'],
  },
};

export function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } });
}

/** The page as the server splices it: the viewer in a JSON tag and on <html>. */
export function splice(bootstrap: Bootstrap): void {
  const tag = document.createElement('script');
  tag.id = 'bootstrap';
  tag.type = 'application/json';
  tag.textContent = JSON.stringify(bootstrap);
  document.head.append(tag);
  document.documentElement.dataset.session = bootstrap.viewer ? 'in' : 'out';
  document.documentElement.dataset.perms = bootstrap.viewer?.permissions.join(' ') ?? '';
}

type Route = (body: unknown) => Response;

/** A stand-in server for `fetch`: answers `METHOD /path` from `routes` (404 otherwise), and keeps
 * every request it got with its JSON body. */
export function server(routes: Record<string, Route>) {
  const calls: { route: string; body: unknown }[] = [];
  const fetch = async (request: Request) => {
    const route = `${request.method} ${new URL(request.url).pathname}`;
    const text = await request.text();
    const body: unknown = text ? JSON.parse(text) : null;
    calls.push({ route, body });
    return routes[route]?.(body) ?? json({ error: 'not_found', message: 'There is no such API.' }, 404);
  };
  return { fetch, calls };
}

/** `location` for an island: the page's address, and `assign` recorded. */
export function at(path: string) {
  const url = new URL(path, 'http://localhost');
  const assign = (href: string) => assigned.push(href);
  const assigned: string[] = [];
  return {
    location: { href: url.href, origin: url.origin, host: url.host, pathname: url.pathname, search: url.search, hash: url.hash, assign },
    assigned,
  };
}
