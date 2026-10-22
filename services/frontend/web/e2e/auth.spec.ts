// The auth journeys in a browser: logging in (password and OAuth) lands on `next`, signing up,
// the emailed links, an invite opened while logged out, and logging out. Against staging, the
// journeys that would make accounts or need a real emailed code don't run.
import { expect, type Browser, type Page } from '@playwright/test';
import { STAGING, USERS, expectAccessible, logIn, shownHeight, test } from './site';

async function fillLogin(page: Page, who: 'plain' | 'all') {
  await page.getByLabel('Username').fill(USERS[who].username);
  await page.getByLabel('Password').fill(USERS[who].password);
  await page.getByRole('button', { name: 'Log in' }).click();
}

test('a password login lands on the page it was sent from', async ({ page }) => {
  await page.goto('/login?next=%2Fasshole');
  await fillLogin(page, 'plain');
  await page.waitForURL('**/asshole');
  await expect(page.locator('html')).toHaveAttribute('data-session', 'in');
});

test('a wrong password says so, and a hostile next is ignored', async ({ page }) => {
  await page.goto('/login?next=%2F%2Fevil.com');
  await page.getByLabel('Username').fill(USERS.plain.username);
  await page.getByLabel('Password').fill('not the password');
  await page.getByRole('button', { name: 'Log in' }).click();
  await expect(page.getByRole('alert')).toBeVisible();
  await expectAccessible(page);
  await fillLogin(page, 'plain');
  await page.waitForURL((url) => url.pathname === '/');
  expect(new URL(page.url()).host).toBe(new URL(test.info().project.use.baseURL!).host);
});

test('a logged-in visitor of the login pages goes home', async ({ context, page }) => {
  await logIn(context, 'plain');
  for (const path of ['/login', '/register', '/forgot-password']) {
    await page.goto(path);
    await expect(page).toHaveURL(/\/$/);
  }
});

test("an emailed link's code goes in a request body, never in a URL; an expired one says so", async ({ page }) => {
  const code = 'e2e-not-a-real-code';
  const urls: string[] = [];
  page.on('request', (r) => urls.push(r.url()));
  await page.goto(`/verify-email#${code}`);
  await expect(page.getByText('This link has expired or was already used. Ask for a new one.')).toBeVisible();
  await expectAccessible(page);
  expect(urls.filter((u) => u.includes(code))).toEqual([]);
  expect(await page.locator('meta[name="referrer"]').getAttribute('content')).toBe('no-referrer');
});

/** The height of the page's column (Panel), as `shownHeight` measures it. */
const panelHeight = (browser: Browser, path: string, ready: string | null) =>
  shownHeight(browser, path, 'main [data-panel]', ready);

test("each form's placeholder is the form's size, so nothing moves when the form arrives", async ({ browser }) => {
  for (const path of ['/login', '/register', '/forgot-password', '/reset-password#code']) {
    expect(await panelHeight(browser, path, null), path).toBeCloseTo(await panelHeight(browser, path, 'form'), 0);
  }
});

// The stand-ins only: these make accounts or need emailed codes.
if (!STAGING) {
  test('an OAuth login carries next through the provider', async ({ page }) => {
    await page.goto('/login?next=%2Fasshole');
    await page.getByRole('link', { name: 'Log in with GitHub' }).click();
    await page.waitForURL('**/asshole');
    await expect(page.locator('html')).toHaveAttribute('data-session', 'in');
    await expect(page.getByRole('button', { name: 'Account: Tess All' })).toBeVisible();
  });

  test('signing up logs in and lands on next; a taken name is marked on its field', async ({ page }) => {
    await page.goto('/register?next=%2Fasshole');
    await page.getByLabel('Email').fill('new@example.test');
    await page.getByLabel('Username').fill('taken');
    await page.getByLabel('Password').fill('long enough');
    await page.getByRole('button', { name: 'Sign up with email' }).click();
    await expect(page.getByText('That username is already in use.')).toBeVisible();
    await expectAccessible(page);
    await page.getByLabel('Username').fill('newcomer');
    await page.getByRole('button', { name: 'Sign up with email' }).click();
    await page.waitForURL('**/asshole');
    await expect(page.locator('html')).toHaveAttribute('data-session', 'in');
  });


  test('forgot password, then the emailed reset link', async ({ page }) => {
    await page.goto('/forgot-password');
    await page.getByLabel('Username or email').fill('tester-none');
    await page.getByRole('button', { name: 'Email me a link' }).click();
    await expect(page.getByText(/we've sent it a link/)).toBeVisible();

    await page.goto('/reset-password#good');
    await page.getByLabel('New password').fill('a new password');
    await page.getByLabel('Type it again').fill('a new password');
    await page.getByRole('button', { name: 'Change password' }).click();
    await expect(page.getByText(/logged out everywhere/)).toBeVisible();
    await expectAccessible(page);
  });



  test("the emailed links' and an invite's placeholders are the size of what most often comes back", async ({
    browser,
  }) => {
    const answers: [string, string][] = [
      ['/verify-email#good', 'a[href="/"]'],
      ['/invite#join', 'a[href="/login"]'],
      ['/delete-account#good', 'button'],
    ];
    for (const [path, ready] of answers) {
      expect(await panelHeight(browser, path, null), path).toBeCloseTo(await panelHeight(browser, path, ready), 0);
    }
  });

  test('opening a confirm link confirms', async ({ page }) => {
    await page.goto('/verify-email#good');
    await expect(page.getByText(/is\s+confirmed/)).toBeVisible();
    await expectAccessible(page);
  });

  test('a delete link deletes on the button, logged out', async ({ page }) => {
    await page.goto('/delete-account#good');
    await expect(page.getByText('tester-none', { exact: true })).toBeVisible();
    await expectAccessible(page);
    await page.getByRole('button', { name: 'Delete my account' }).click();
    await page.waitForURL('**/account-deleted');
  });

  test('an invite opened logged out joins after logging in, and comes back', async ({ page }) => {
    await page.goto('/invite#join');
    await expect(page.getByText('the Arcane dice test group')).toBeVisible();
    await expectAccessible(page);
    await page.getByRole('link', { name: 'I have an account: log in' }).click();
    await fillLogin(page, 'plain');
    await page.waitForURL('**/invite#join');
    await expect(page.getByRole('link', { name: 'Open Arcane dice' })).toBeVisible();
  });

  test('an invite whose group has since gained admin rights is refused, with why', async ({ context, page }) => {
    await logIn(context, 'plain');
    await page.goto('/invite#admin');
    await expect(page.getByText(/admin rights, which links never give/)).toBeVisible();
    await expectAccessible(page);
  });
}
