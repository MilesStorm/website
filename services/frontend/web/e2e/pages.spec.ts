// Every page for every kind of viewer: served by the server from Astro's build, with the viewer
// on <html> before any script runs, and accessible.
import { expect, test } from '@playwright/test';
import { PAGES, expectAccessible, logIn, type Who } from './site';

const VIEWERS: Who[] = ['anonymous', 'plain', 'all'];

for (const who of VIEWERS) {
  for (const { path, access } of PAGES) {
    test(`${path} as ${who}`, async ({ context, page }) => {
      expect(access).toBe('public');
      await logIn(context, who);
      // The countdown reloads the page when it draws under 30 days (about one in twelve): a
      // fixed draw keeps it still while the page is checked.
      if (path === '/asshole') await page.addInitScript(() => (Math.random = () => 0.5));
      const res = await page.goto(path);
      expect(res?.status()).toBe(200);
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

test('Astro files are cached for good, and only they are served from the build', async ({ page, request }) => {
  const res = await page.goto('/');
  const css = (await res!.text()).match(/href="(\/_astro\/[^"]+\.css)"/)?.[1];
  expect(css).toBeTruthy();
  const asset = await request.get(css!);
  expect(asset.status()).toBe(200);
  expect(asset.headers()['cache-control']).toBe('public, max-age=31536000, immutable');
  // These go to the Dioxus app (its "not found" page), never to a file of the Astro build. (A
  // path through `/_astro/..` is the server's own test: the browser would tidy it away.)
  for (const path of ['/index.html', '/asshole.html', '/404.html']) {
    const body = await (await request.get(path)).text();
    expect(body).not.toContain('bff-inject');
    expect(body).not.toContain('id="bootstrap"');
  }
});
