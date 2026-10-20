// Every page for every kind of viewer, as its access in PAGES says: served by the server from
// Astro's build with the viewer on <html> before any script runs, and accessible; or a redirect
// (to log in, or home from a guest-only page), the 403 page or the 503 page.
import { readFileSync } from 'node:fs';
import { expect, test } from '@playwright/test';
import { PAGES, STAGING, expectAccessible, logIn, type Access, type Who } from './site';

// Auth being down can't be arranged on staging.
const VIEWERS: Who[] = STAGING ? ['anonymous', 'plain', 'all'] : ['anonymous', 'plain', 'all', 'down'];

/** What a page of `access` at `path` answers `who`, written out by hand: the status, and where
 * a redirect goes. */
function expected(access: Access, who: Who, path: string): { status: number; location?: string } {
  if (access === 'public') return { status: 200 };
  if (access === 'guest_only') return who === 'anonymous' ? { status: 200 } : { status: 303, location: '/' };
  if (who === 'anonymous') return { status: 303, location: `/login?next=${path}` };
  if (who === 'down') return { status: 503 };
  if (access === 'logged_in' || who === 'all') return { status: 200 };
  return { status: 403 };
}

for (const who of VIEWERS) {
  for (const { path, access } of PAGES) {
    const want = expected(access, who, path);
    test(`${path} as ${who}: ${want.status}`, async ({ context, page }) => {
      await logIn(context, who);
      if (want.location) {
        const res = await context.request.get(path, { maxRedirects: 0 });
        expect(res.status()).toBe(want.status);
        expect(res.headers()['location']).toBe(want.location);
        return;
      }
      // The countdown reloads the page when it draws under 30 days (about one in twelve): a
      // fixed draw keeps it still while the page is checked.
      if (path === '/asshole') await page.addInitScript(() => (Math.random = () => 0.5));
      const res = await page.goto(path);
      expect(res?.status()).toBe(want.status);
      expect(res?.headers()['cache-control']).toBe('private, no-cache');
      const html = await res!.text();
      // Spliced: the marker is gone, the viewer is there.
      expect(html).not.toContain('name="bff-inject"');
      expect(html).toMatch(/<script id="bootstrap" type="application\/json">/);
      await expect(page.locator('html')).toHaveAttribute('data-session', who === 'anonymous' ? 'out' : 'in');
      await expect(page.getByRole('main')).toBeVisible();
      await expectAccessible(page);
    });
  }
}

// The 403 and 503 pages as the image has them (e2e/serve.sh copies them out of it), shown at the
// server's address so their styles and scripts come from the server. They are checked here as
// well as above because no page in PAGES is guarded yet, so no request reaches them.
if (!STAGING) {
  for (const [file, heading] of [
    ['403.html', "You don't have access to this page"],
    ['503.html', "Can't check your access right now"],
  ]) {
    test(`the status page ${file} is accessible`, async ({ page }) => {
      const body = readFileSync(new URL(`.status/${file}`, import.meta.url), 'utf8');
      await page.route('**/status-page', (route) =>
        route.fulfill({ status: 200, contentType: 'text/html; charset=utf-8', body }),
      );
      await page.goto('/status-page');
      await expect(page.getByRole('heading', { name: heading })).toBeVisible();
      await expect(page.getByRole('link', { name: 'Go home' })).toBeVisible();
      await expectAccessible(page);
    });
  }
}

test('Astro files are cached for good, and only they are served from the build', async ({ page, request }) => {
  const res = await page.goto('/');
  const css = (await res!.text()).match(/href="(\/_astro\/[^"]+\.css)"/)?.[1];
  expect(css).toBeTruthy();
  const asset = await request.get(css!);
  expect(asset.status()).toBe(200);
  expect(asset.headers()['cache-control']).toBe('public, max-age=31536000, immutable');
  // These go to the Dioxus app (its "not found" page), never to a file of the Astro build. (A
  // path through `/_astro/..` is the server's own test: the browser would tidy it away.)
  for (const path of ['/index.html', '/asshole.html', '/404.html', '/403.html', '/503.html']) {
    const body = await (await request.get(path)).text();
    expect(body).not.toContain('bff-inject');
    expect(body).not.toContain('id="bootstrap"');
  }
});
