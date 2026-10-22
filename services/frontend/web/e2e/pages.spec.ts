// Every page for every kind of viewer, as its access in PAGES says: served by the server from
// Astro's build with the viewer on <html> before any script runs, and accessible; or a redirect
// (to log in, or home from a guest-only page), the 403 page or the 503 page.
import { expect } from '@playwright/test';
import { PAGES, STAGING, expectAccessible, logIn, test, type Access, type Who } from './site';

// No page gets a camera: on /arcane the browser refuses it, so no frame goes anywhere (staging's
// ai_pipeline included).
test.use({ permissions: [] });
test.beforeEach(async ({ context }) => {
  await context.addInitScript(() => {
    if (navigator.mediaDevices) {
      navigator.mediaDevices.getUserMedia = () => Promise.reject(new DOMException('Denied by the test', 'NotAllowedError'));
    }
  });
});

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
      // The status pages, through a real request: the guard's answer, spliced like any page.
      if (want.status === 403) await expect(page.getByRole('heading', { name: "You don't have access to this page" })).toBeVisible();
      if (want.status === 503) {
        await expect(page.getByRole('heading', { name: "Can't check your access right now" })).toBeVisible();
        expect(res?.headers()['retry-after']).toBe('30');
      }
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
  // These are the 404 page, never a file of the Astro build. (A path through `/_astro/..` is the
  // server's own test: the browser would tidy it away.)
  for (const path of ['/index.html', '/asshole.html', '/404.html', '/403.html', '/503.html']) {
    const res = await request.get(path);
    expect(res.status(), path).toBe(404);
    const body = await res.text();
    expect(body).not.toContain('bff-inject');
    expect(body).toContain('Page not found');
  }
});

test('any other path is the 404 page, for every kind of viewer', async ({ context, page }) => {
  for (const who of ['anonymous', 'plain'] as const) {
    await logIn(context, who);
    const res = await page.goto('/no/such/page');
    expect(res?.status()).toBe(404);
    expect(res?.headers()['cache-control']).toBe('private, no-cache');
    await expect(page.getByRole('heading', { name: 'Page not found' })).toBeVisible();
    await expect(page.locator('html')).toHaveAttribute('data-session', who === 'anonymous' ? 'out' : 'in');
    await expectAccessible(page);
  }
});

test('what isn’t a page load gets no page: the icon, unknown API paths, unknown files', async ({ request }) => {
  const icon = await request.get('/favicon.ico');
  expect(icon.status()).toBe(200);
  expect(icon.headers()['content-type']).toBe('image/x-icon');
  for (const path of ['/api/nope', '/bff/nope', '/api/', '/bff/']) {
    const res = await request.get(path);
    expect(res.status(), path).toBe(404);
    expect((await res.json()).error, path).toBe('not_found');
  }
  // Behind Cloudflare, Cloudflare answers /robots.txt itself (its managed robots.txt).
  if (!STAGING) {
    const file = await request.get('/robots.txt');
    expect(file.status()).toBe(404);
    expect(await file.text()).toBe('');
  }
});
