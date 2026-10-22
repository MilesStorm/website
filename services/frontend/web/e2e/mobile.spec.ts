// The menus on a phone, by touch (the `pixel` and `iphone` projects): a tap opens them.
import { expect } from '@playwright/test';
import { expectAccessible, logIn, test } from './site';

// iOS Safari gives the page no touch pointer events while the only listener for them is React's,
// on an island's `display: contents` root (facebook/react#29890); the tap's click still arrives.
// Playwright's WebKit isn't iOS and delivers them, so this holds them back as iOS does, until a
// Radix menu is open (it listens on the document itself, and then they come).
test.beforeEach(async ({ page }, info) => {
  if (info.project.name !== 'iphone') return;
  await page.addInitScript(() => {
    window.addEventListener(
      'pointerdown',
      (e) => {
        if (e.pointerType === 'touch' && !document.querySelector('[role="menu"]')) e.stopImmediatePropagation();
      },
      true,
    );
  });
});

test('a tap on the picture opens the user menu, and Log out logs out', async ({ context, page }) => {
  await logIn(context, 'plain');
  await page.goto('/profile');
  await page.getByRole('button', { name: /^Account: / }).tap();
  await page.getByRole('menuitem', { name: 'Log out' }).tap();
  await expect(page).toHaveURL(/\/$/);
  await expect(page.locator('html')).toHaveAttribute('data-session', 'out');
});

test("the profile page's Log out logs out", async ({ context, page }) => {
  await logIn(context, 'plain');
  await page.goto('/profile');
  await page.getByRole('main').getByRole('button', { name: 'Log out' }).tap();
  await expect(page).toHaveURL(/\/$/);
  await expect(page.locator('html')).toHaveAttribute('data-session', 'out');
});

test('a tap on the theme button opens its menu', async ({ page }) => {
  await page.emulateMedia({ colorScheme: 'light' });
  await page.goto('/');
  await page.getByRole('button', { name: 'Theme' }).tap();
  await page.getByRole('menuitemradio', { name: 'Dark' }).tap();
  await expect(page.locator('html')).toHaveClass(/\bdark\b/);
});

test("a tap on the admin panel's Add opens its menu", async ({ context, page }) => {
  await logIn(context, 'all');
  await page.goto('/admin');
  await page.getByRole('button', { name: /^Add role to / }).first().tap();
  await expect(page.getByRole('menu')).toBeVisible();
  await expect(page.getByRole('menuitem').first()).toBeVisible();
});

for (const scheme of ['light', 'dark'] as const) {
  test(`the navbar is accessible, ${scheme}`, async ({ context, page }) => {
    await logIn(context, 'all');
    await page.emulateMedia({ colorScheme: scheme });
    await page.goto('/');
    await page.getByRole('navigation', { name: 'Main' }).getByLabel('Menu').tap();
    await expect(page.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: 'Admin' })).toBeVisible();
    await expectAccessible(page);
  });
}
