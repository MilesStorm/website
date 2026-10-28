// The Enshrouded panel in a browser. Against staging it is only looked at, never a Restart or
// Force update: those act on the real game server.
import { expect } from '@playwright/test';
import { STAGING, expectAccessible, logIn, shownHeight, test } from './site';

test('the panel shows the server, in light and dark', async ({ context, page }) => {
  await logIn(context, 'all');
  for (const colorScheme of ['light', 'dark'] as const) {
    await page.emulateMedia({ colorScheme });
    await page.goto('/enshrouded');
    if (colorScheme === 'dark') await expect(page.locator('html')).toHaveClass(/\bdark\b/);
    else await expect(page.locator('html')).not.toHaveClass(/\bdark\b/);
    await expect(page.getByRole('heading', { name: 'Chronicle' })).toBeVisible();
    await expect(page.getByRole('heading', { name: 'Players online' })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Restart' })).toBeVisible();
    await expectAccessible(page);
  }
});

if (!STAGING) {
  // The stand-ins' usual answer (online, nobody waiting on a command), and with Steam's latest
  // build unknown; on a wide window, where the lists sit beside each other (1024), at the
  // breakpoint where the cards drop to their short labels and notes, and on a phone.
  for (const width of [1280, 1024, 640, 390]) {
    test.describe(`at ${width}px`, () => {
      test.use({ viewport: { width, height: 900 } });
      for (const fresh of [false, 'nobuild']) {
        test(`the placeholder is the heading's, figures' and controls' size${fresh ? ', with the latest build unknown' : ''}`, async ({
          browser,
          viewport,
        }) => {
          expect(viewport?.width).toBe(width);
          const height = (ready: string | null) =>
            shownHeight(browser, '/enshrouded', '[data-overview]', ready, 'all', fresh, viewport);
          expect(await height(null)).toBeCloseTo(await height('[data-slot="badge"]'), 0);
        });
      }
    });
  }

  test('a restart asks first, then shows in the recent commands', async ({ context, page }) => {
    await logIn(context, 'all', true);
    await page.goto('/enshrouded');
    await page.getByRole('button', { name: 'Restart' }).click();
    await expect(page.getByText('1 player is online. They will be disconnected.')).toBeVisible();
    await expectAccessible(page);
    await page.getByRole('button', { name: 'Yes, restart' }).click();
    await expect(page.getByText('Restart sent. The status above follows along.')).toBeVisible();
    const me = page.getByRole('listitem').filter({ hasText: /^tester-all-\w+ restarted the server/ });
    await expect(me.first()).toBeVisible();
  });

  test('with a player on, Force update can still go now', async ({ context, page }) => {
    await logIn(context, 'all', true);
    await page.goto('/enshrouded');
    await page.getByRole('button', { name: 'Force update' }).click();
    await expect(page.getByText(/Force update waits until everyone has left/)).toBeVisible();
    await page.getByRole('button', { name: 'Update now anyway' }).hover();
    // The hover colour fades in (the button's 150 ms transition).
    await page.waitForTimeout(300);
    await expectAccessible(page);
    await page.getByRole('button', { name: 'Update now anyway' }).click();
    await expect(page.getByText('Update started. The status above follows along.')).toBeVisible();
  });

  test("a command already under way says so (auth's 409)", async ({ context, page }) => {
    await logIn(context, 'all', 'busy');
    await page.goto('/enshrouded');
    await page.getByRole('button', { name: 'Restart' }).click();
    await page.getByRole('button', { name: 'Yes, restart' }).click();
    await expect(page.getByText('A restart or update is already under way.')).toBeVisible();
    await expectAccessible(page);
  });
}
