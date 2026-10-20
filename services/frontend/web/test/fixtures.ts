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
