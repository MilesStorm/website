// The cookie notice sits over the bottom of the window until it is answered: on a phone the page
// ends that much lower, so its last controls can still be scrolled to and used.
import { expect } from '@playwright/test';
import { logIn, test } from './site';

test.use({ viewport: { width: 390, height: 844 } });

test('with the cookie notice open, the last control of the profile can be reached', async ({ context, page }) => {
  await logIn(context, 'plain');
  await page.goto('/profile');
  await expect(page.getByRole('region', { name: 'Cookies' })).toBeVisible();
  // The theme card ends the page. A click fails if the notice is over the button.
  const theme = page.getByRole('main').getByRole('button', { name: 'Theme' });
  await theme.scrollIntoViewIfNeeded();
  await theme.click();
  await expect(page.getByRole('menuitemradio', { name: 'Dark' })).toBeVisible();
});
