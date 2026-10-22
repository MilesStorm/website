// The navbar, theme and cookie notice: right on the first paint, without waiting for scripts.
import { expect } from '@playwright/test';
import { logIn, test } from './site';

test.describe('without JavaScript the navbar is still right', () => {
  test.use({ javaScriptEnabled: false });

  test('logged out: log in and register, no tools', async ({ page }) => {
    await page.goto('/');
    const nav = page.getByRole('navigation', { name: 'Main' });
    await expect(nav.getByRole('link', { name: 'Log in' })).toBeVisible();
    await expect(nav.getByRole('link', { name: 'Admin' })).toBeHidden();
    await expect(page.getByRole('heading', { name: 'Tools for game nights with friends.' })).toBeVisible();
  });

  test('logged in without permissions: no log in, no tools', async ({ context, page }) => {
    await logIn(context, 'plain');
    await page.goto('/');
    const nav = page.getByRole('navigation', { name: 'Main' });
    await expect(nav.getByRole('link', { name: 'Log in' })).toBeHidden();
    for (const tool of ['Ark', 'Enshrouded', 'Arcane', 'Admin']) {
      await expect(nav.getByRole('link', { name: tool, exact: true })).toBeHidden();
    }
  });

  test('logged in with every permission: every tool', async ({ context, page }) => {
    await logIn(context, 'all');
    await page.goto('/');
    const nav = page.getByRole('navigation', { name: 'Main' });
    for (const tool of ['Ark', 'Enshrouded', 'Arcane', 'Admin']) {
      await expect(nav.getByRole('link', { name: tool, exact: true })).toBeVisible();
    }
    await expect(nav.getByRole('link', { name: 'Home', exact: true })).toHaveAttribute('aria-current', 'page');
    await page.goto('/admin');
    await expect(nav.getByRole('link', { name: 'Admin', exact: true })).toHaveAttribute('aria-current', 'page');
    await expect(nav.getByRole('link', { name: 'Home', exact: true })).not.toHaveAttribute('aria-current');
  });
});

test('the logged-in start page greets the user', async ({ context, page }) => {
  await logIn(context, 'all');
  await page.goto('/');
  await expect(page.getByRole('button', { name: /^Account: / })).toBeVisible();
  await expect(page.getByText('Welcome back')).toBeVisible();
});

test.describe('the theme follows the system until one is chosen', () => {
  test('light system', async ({ page }) => {
    await page.emulateMedia({ colorScheme: 'light' });
    await page.goto('/');
    await expect(page.locator('html')).not.toHaveClass(/\bdark\b/);
  });

  test('dark system', async ({ page }) => {
    await page.emulateMedia({ colorScheme: 'dark' });
    await page.goto('/');
    await expect(page.locator('html')).toHaveClass(/\bdark\b/);
  });

  test('a chosen theme wins, and the old saved theme is dropped once', async ({ page }) => {
    await page.emulateMedia({ colorScheme: 'light' });
    await page.addInitScript(() => {
      if (!sessionStorage.getItem('seeded')) {
        localStorage.setItem('theme', 'a0c1');
        sessionStorage.setItem('seeded', '1');
      }
    });
    await page.goto('/');
    expect(await page.evaluate(() => localStorage.getItem('theme'))).toBeNull();
    await page.getByRole('button', { name: 'Theme' }).click();
    await page.getByRole('menuitemradio', { name: 'Dark' }).click();
    await page.reload();
    await expect(page.locator('html')).toHaveClass(/\bdark\b/);
  });
});

test('the cookie notice goes once accepted', async ({ page }) => {
  await page.goto('/');
  const notice = page.getByRole('region', { name: 'Cookies' });
  await notice.getByRole('button', { name: 'Accept' }).click();
  await expect(notice).toBeHidden();
  await page.reload();
  // The notice is a client-only island: hidden proves nothing until every island has started.
  await page.waitForFunction(() => document.querySelector('astro-island[ssr]') === null);
  await expect(notice).toBeHidden();
});

test('an island whose script a deploy removed reloads the page once, then gives up', async ({ page }) => {
  // Astro's retry adds a query (`?astro-retry=…`): gone for that too.
  await page.route(/\/_astro\/ModeToggle\.[^/?]+\.js(\?.*)?$/, (route) => route.fulfill({ status: 404 }));
  let loads = 0;
  page.on('load', () => loads++);
  await page.goto('/');
  // Astro retries the import after a second; then the guard reloads, and the second failure is
  // left alone (the same page and release).
  await expect.poll(() => loads, { timeout: 10_000 }).toBe(2);
  await page.waitForTimeout(3_000);
  expect(loads).toBe(2);
  await expect(page.getByRole('heading', { name: 'Tools for game nights with friends.' })).toBeVisible();
});
