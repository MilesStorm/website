// The profile and the Ark panel in a browser. Against staging only what changes nothing runs:
// looking at them. Changing a name or picture, sharing, and Ark commands (which would start or
// stop the real game server) run against the stand-ins only.
import { expect, test } from '@playwright/test';
import { STAGING, expectAccessible, logIn } from './site';

/** A 2×2 PNG: small, and a picture the server can read. */
const PNG = Buffer.from(
  'iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAEUlEQVR4nGP4z8DA8B+MgBgAHfAD/dPQfSYAAAAASUVORK5CYII=',
  'base64',
);

test('the profile shows the account, and its theme card works', async ({ context, page }) => {
  await logIn(context, 'plain');
  await page.goto('/profile');
  await expect(page.getByRole('heading', { name: 'Account', exact: true })).toBeVisible();
  await expect(page.getByRole('heading', { name: 'Email' })).toBeVisible();
  await expect(page.getByRole('heading', { name: 'Delete account' })).toBeVisible();
  await expectAccessible(page);
  await page.getByRole('main').getByRole('button', { name: 'Theme' }).click();
  await page.getByRole('menuitemradio', { name: 'Light' }).click();
  await expect(page.locator('html')).not.toHaveClass(/\bdark\b/);
});

test('the Ark panel shows who is online', async ({ context, page }) => {
  await logIn(context, 'all');
  await page.goto('/ark');
  await expect(page.getByRole('heading', { name: 'Players online' })).toBeVisible();
  await expect(page.getByText(/Server is active|Server running, no players|The game server is not running\./)).toBeVisible();
  await expectAccessible(page);
});

if (!STAGING) {
  test('a new display name shows on the page and in the user menu', async ({ context, page }) => {
    await logIn(context, 'plain', true);
    await page.goto('/profile');
    await page.getByLabel('Display name').fill('Plain Tester');
    await page.getByRole('button', { name: 'Save' }).click();
    await expect(page.getByText('Saved.')).toBeVisible();
    await expect(page.getByRole('button', { name: 'Account: Plain Tester' })).toBeVisible();
    // Empty: the username again.
    await page.getByLabel('Display name').fill('');
    await page.getByRole('button', { name: 'Save' }).click();
    await expect(page.getByRole('button', { name: /^Account: tester-none-/ })).toBeVisible();
  });

  test('a picture uploads, shows, and goes again', async ({ context, page }) => {
    await logIn(context, 'plain', true);
    await page.goto('/profile');
    await page.getByLabel('Profile picture file').setInputFiles({ name: 'me.png', mimeType: 'image/png', buffer: PNG });
    await expect(page.getByText('Picture updated.')).toBeVisible();
    const src = await page.getByAltText('Your profile picture').getAttribute('src');
    expect(src).toMatch(/^\/api\/account\/picture\?/);
    const picture = await context.request.get(src!);
    expect(picture.status()).toBe(200);
    expect(picture.headers()['content-type']).toBe('image/jpeg');
    await page.getByRole('button', { name: 'Remove' }).click();
    await expect(page.getByText('Picture removed.')).toBeVisible();
  });

  test('an unconfirmed email can be sent its link again', async ({ context, page }) => {
    await logIn(context, 'plain');
    await page.goto('/profile');
    await expect(page.getByText('Not confirmed')).toBeVisible();
    await page.getByRole('button', { name: 'Send confirmation email' }).click();
    await expect(page.getByText(/Sent\. Open the link in the email to none@example\.test/)).toBeVisible();
  });

  test('with the arcane permission, roll sharing can be turned on and off', async ({ context, page }) => {
    await logIn(context, 'all', true);
    await page.goto('/profile');
    const toggle = page.getByRole('switch', { name: 'Share pictures of my rolls' });
    await expect(toggle).toHaveAttribute('aria-checked', 'false');
    await toggle.click();
    await expect(toggle).toHaveAttribute('aria-checked', 'true');
    await page.reload();
    await expect(toggle).toHaveAttribute('aria-checked', 'true');
    await page.getByRole('button', { name: 'Delete my shared pictures' }).click();
    await page.getByRole('button', { name: 'Delete everything' }).click();
    await expect(page.getByText('Done. There was nothing to delete.')).toBeVisible();
    await expect(toggle).toHaveAttribute('aria-checked', 'false');
    await expectAccessible(page);
  });

  test('without it, there is no sharing card', async ({ context, page }) => {
    await logIn(context, 'plain');
    await page.goto('/profile');
    await expect(page.getByRole('heading', { name: 'Account', exact: true })).toBeVisible();
    await expect(page.getByRole('heading', { name: 'Help improve dice recognition' })).toBeHidden();
  });

  test('a confirmed address is emailed a deletion link', async ({ context, page }) => {
    await logIn(context, 'all');
    await page.goto('/profile');
    await page.getByRole('button', { name: 'Delete my account…' }).click();
    await page.getByRole('button', { name: 'Email me the link' }).click();
    await expect(page.getByText('Sent to all@example.test. The link works for 1 hour.')).toBeVisible();
    await expectAccessible(page);
  });

  test('an Ark command says what it did', async ({ context, page }) => {
    await logIn(context, 'all');
    await page.goto('/ark');
    await expect(page.getByText('3', { exact: true })).toBeVisible();
    await page.getByRole('button', { name: 'Start', exact: true }).click();
    await expect(page.getByText('Started.')).toBeVisible();
  });
}
