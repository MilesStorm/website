// Logging out, then Back. Playwright turns the back/forward cache off by default, and its
// headless shell has none: here it is Chromium's full headless mode with the cache on (launch
// options, so a file of its own), so Back really restores the page as it was, logged in, and the
// layout's `pageshow` handler must load it again.
import { expect, test } from '@playwright/test';
import { logIn } from './site';

test.use({ channel: 'chromium', launchOptions: { ignoreDefaultArgs: ['--disable-back-forward-cache'] } });

test('logging out, Back shows the page logged out', async ({ context, page }) => {
  // Every document of the tab: a load counted per path, and a restore from the cache noted.
  await page.addInitScript(() => {
    const key = `loads:${location.pathname}`;
    sessionStorage.setItem(key, String(Number(sessionStorage.getItem(key) ?? 0) + 1));
    addEventListener('pageshow', (e) => {
      if (e.persisted) sessionStorage.setItem('restored', location.pathname);
    });
  });
  await logIn(context, 'plain');
  await page.goto('/account-deleted');
  await expect(page.locator('html')).toHaveAttribute('data-session', 'in');
  // A page left with a request still open (an island's script, the tracing's spans) isn't kept.
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
  const seen = await page.evaluate(() => ({
    restored: sessionStorage.getItem('restored'),
    loads: sessionStorage.getItem('loads:/account-deleted'),
  }));
  // Restored from the cache, then loaded again: the first load and the reload.
  expect(seen).toEqual({ restored: '/account-deleted', loads: '2' });
});
