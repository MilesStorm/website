// Logging out, then Back. Playwright turns the back/forward cache off by default, and its
// headless shell has none: here it is Chromium's full headless mode with the cache on (launch
// options, so a file of its own), so Back really restores the page as it was, logged in, and the
// layout's `pageshow` handler must load it again.
import { expect } from '@playwright/test';
import { STAGING, logIn, test } from './site';

test.use({
  channel: 'chromium',
  launchOptions: {
    ignoreDefaultArgs: ['--disable-back-forward-cache'],
    // The Arcane test's camera: Chromium's fake one.
    args: ['--use-fake-device-for-media-stream', '--use-fake-ui-for-media-stream'],
  },
});

test('logging out, Back shows the page logged out', async ({ context, page }) => {
  // Chromium says why a page wasn't kept: the test fails with its reasons, not just "not restored".
  const notKept: unknown[] = [];
  const cdp = await context.newCDPSession(page);
  await cdp.send('Page.enable');
  cdp.on('Page.backForwardCacheNotUsed', (e) => notKept.push(e.notRestoredExplanations));
  // Every document of the tab: a load counted per path, and a restore from the cache noted.
  await page.addInitScript(() => {
    const key = `loads:${location.pathname}`;
    sessionStorage.setItem(key, String(Number(sessionStorage.getItem(key) ?? 0) + 1));
    addEventListener('pageshow', (e) => {
      if (e.persisted) sessionStorage.setItem('restored', location.pathname);
    });
  });
  await logIn(context, 'plain');
  // A page left with a request still open isn't kept. Where the tracing is on (the staging and
  // production hosts; not the stand-ins' localhost), it sends the page load's spans shortly after
  // the load, to /faro/collect: wait for that answer, then for nothing else in flight.
  const exported = STAGING ? page.waitForResponse((r) => new URL(r.url()).pathname === '/faro/collect') : null;
  await page.goto('/account-deleted');
  await expect(page.locator('html')).toHaveAttribute('data-session', 'in');
  await exported;
  await page.waitForLoadState('networkidle');
  await page.goto('/');
  await page.getByRole('button', { name: /^Account: / }).click();
  await page.getByRole('menuitem', { name: 'Log out' }).click();
  await expect(page.locator('html')).toHaveAttribute('data-session', 'out');

  // A page restored from the cache fires no `load`: wait only for the navigation itself.
  await page.goBack({ waitUntil: 'commit' });
  await expect(page).toHaveURL(/\/account-deleted$/);
  await expect(page.locator('html')).toHaveAttribute('data-session', 'out');
  await expect(page.getByRole('link', { name: 'Log in' }).first()).toBeVisible();
  expect(notKept).toEqual([]);
  const seen = await page.evaluate(() => ({
    restored: sessionStorage.getItem('restored'),
    loads: sessionStorage.getItem('loads:/account-deleted'),
  }));
  // Restored from the cache, then loaded again: the first load and the reload.
  expect(seen).toEqual({ restored: '/account-deleted', loads: '2' });
});

// The camera, not on staging (its frames would reach the real ai_pipeline).
if (!STAGING) {
  test('leaving the Arcane page stops the camera', async ({ context, page }) => {
    await context.grantPermissions(['camera']);
    await logIn(context, 'all');
    await page.goto('/arcane');
    await expect(page.getByRole('status', { name: 'Connection' })).toHaveText('Connected');
    // Noted after the page's own `pagehide` handler (registered first), in this tab's storage.
    await page.evaluate(() => {
      const video = document.querySelector('video')!;
      const tracks = (video.srcObject as MediaStream).getTracks();
      addEventListener('pagehide', () => {
        sessionStorage.setItem('camera', JSON.stringify(tracks.map((t) => t.readyState)));
      });
    });
    await page.goto('/account-deleted');
    expect(await page.evaluate(() => sessionStorage.getItem('camera'))).toBe('["ended"]');
  });
}
