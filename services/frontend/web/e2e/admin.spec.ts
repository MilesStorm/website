// The admin panel in a browser. Against staging it is only looked at: no role, permission or
// invite link is changed there.
import { expect, test, type Page } from '@playwright/test';
import { STAGING, expectAccessible, logIn, shownHeight } from './site';

async function tab(page: Page, name: 'Users' | 'Roles' | 'Invites') {
  await page.getByRole('tab', { name }).click();
}

/** Gives `owner` one more role or permission, from its add menu. */
async function add(page: Page, kind: 'role' | 'permission', owner: string, name: string) {
  await page.getByRole('button', { name: `Add ${kind} to ${owner}` }).click();
  await page.getByRole('menuitem', { name, exact: true }).click();
}

test('the panel lists users, roles and invite links, in light and dark', async ({ context, page }) => {
  await logIn(context, 'all');
  for (const colorScheme of ['light', 'dark'] as const) {
    await page.emulateMedia({ colorScheme });
    await page.goto('/admin');
    if (colorScheme === 'dark') await expect(page.locator('html')).toHaveClass(/\bdark\b/);
    else await expect(page.locator('html')).not.toHaveClass(/\bdark\b/);
    await expect(page.getByRole('columnheader', { name: 'Username' })).toBeVisible();
    await expect(page.getByText(/^\d+ users?$/)).toBeVisible();
    await expectAccessible(page);
    await tab(page, 'Roles');
    await expect(page.getByRole('columnheader', { name: 'Permissions' })).toBeVisible();
    await expectAccessible(page);
    await tab(page, 'Invites');
    await expect(page.getByRole('heading', { name: 'New invite link' })).toBeVisible();
    await expectAccessible(page);
  }
});

test('the user list can be searched', async ({ context, page }) => {
  const me = await logIn(context, 'all');
  await page.goto('/admin');
  await page.getByLabel('Search users').fill(me);
  await expect(page.getByRole('cell', { name: me, exact: true })).toBeVisible();
  await expect(page.getByText(/^\d+ users?$/)).toHaveText(/^[1-9]\d* users?$/);
});

if (!STAGING) {
  test('a role is given to a user and taken away', async ({ browser, context, page }) => {
    // Someone new to give it to: an account of their own, logged in once.
    const other = await browser.newContext({ baseURL: test.info().project.use.baseURL, ignoreHTTPSErrors: true });
    const them = await logIn(other, 'plain', true);
    await other.close();
    await logIn(context, 'all');
    await page.goto('/admin');
    await page.getByLabel('Search users').fill(them);
    await expect(page.getByRole('cell', { name: them, exact: true })).toBeVisible();
    await add(page, 'role', them, 'arcane_user');
    const remove = page.getByRole('button', { name: `Remove role arcane_user from ${them}` });
    await expect(remove).toBeVisible();
    await expectAccessible(page);
    await remove.click();
    await expect(remove).toBeHidden();
  });

  test('a permission is granted to a role and revoked; one this site does not use is marked', async ({ context, page }) => {
    const me = await logIn(context, 'all', true);
    const role = `scratch_${me.split('-').at(-1)}`;
    await page.goto('/admin');
    await tab(page, 'Roles');
    await expect(page.getByRole('row', { name: /^admin / }).getByText('(unused)')).toBeVisible();
    await page.getByLabel('Search roles').fill(role);
    await add(page, 'permission', role, 'llama');
    const revoke = page.getByRole('button', { name: `Remove permission llama from ${role}` });
    await expect(revoke).toBeVisible();
    await revoke.click();
    await expect(revoke).toBeHidden();
  });

  test('taking admin rights away from yourself asks first, and says why', async ({ context, page }) => {
    const me = await logIn(context, 'all', true);
    const role = `scratch_${me.split('-').at(-1)}`;
    await page.goto('/admin');
    await page.getByLabel('Search users').fill(me);
    await page.getByRole('button', { name: `Remove role admin from ${me}` }).click();
    const own = page.getByRole('alertdialog', { name: `Remove admin from ${me}?` });
    await expect(own.getByText(/This is your own account/)).toBeVisible();
    await expectAccessible(page);
    await own.getByRole('button', { name: 'Keep' }).click();
    await expect(page.getByRole('button', { name: `Remove role admin from ${me}` })).toBeVisible();

    // manage_permissions, on the test's own role: asks, then goes.
    await page.getByRole('tab', { name: 'Roles' }).click();
    await page.getByLabel('Search roles').fill(role);
    await add(page, 'permission', role, 'manage_permissions');
    await page.getByRole('button', { name: `Remove permission manage_permissions from ${role}` }).click();
    const grant = page.getByRole('alertdialog', { name: `Remove manage_permissions from ${role}?` });
    await expect(grant.getByText(/manage_permissions is what lets anyone use this page/)).toBeVisible();
    await grant.getByRole('button', { name: 'Remove' }).click();
    await expect(page.getByRole('button', { name: `Remove permission manage_permissions from ${role}` })).toBeHidden();
  });

  test('an invite link is made, shown once, listed and turned off', async ({ context, page }) => {
    await logIn(context, 'all');
    const note = `e2e ${Math.random().toString(36).slice(2, 10)}`;
    await page.goto('/admin');
    await tab(page, 'Invites');
    await page.getByLabel("Note (who it's for)").fill(note);
    await page.getByRole('button', { name: 'Make link' }).click();
    await expect(page.getByLabel('New invite link')).toHaveValue(/\/invite#/);
    const row = page.getByRole('row').filter({ hasText: note });
    await expect(row.getByText('Active')).toBeVisible();
    await row.getByRole('button', { name: `Turn off ${note}` }).click();
    await expect(page.getByRole('alertdialog', { name: 'Turn off this link?' })).toBeVisible();
    await expectAccessible(page);
    await page.getByRole('button', { name: 'Yes, turn off' }).click();
    await expect(row.getByText('Turned off')).toBeVisible();
  });

  test('a link for a role with admin rights is refused, with why', async ({ context, page }) => {
    await logIn(context, 'all');
    await page.goto('/admin');
    await tab(page, 'Invites');
    await page.getByLabel('Gives the role').selectOption('admin');
    await page.getByRole('button', { name: 'Make link' }).click();
    await expect(page.getByText(/Links can't give a role with admin rights/)).toBeVisible();
    await expectAccessible(page);
  });

  // The stand-ins' users fill more than a page: the placeholder's usual answer.
  test("the panel's placeholder is the panel's size, so nothing moves when it arrives", async ({ browser }) => {
    const height = (ready: string | null) => shownHeight(browser, '/admin', 'main', ready, 'all');
    expect(await height(null)).toBeCloseTo(await height('nav[aria-label="Pages"]'), 0);
  });

  test('the user list has pages', async ({ context, page }) => {
    await logIn(context, 'all');
    await page.goto('/admin');
    const pages = page.getByRole('navigation', { name: 'Pages' });
    await expect(pages.getByRole('button', { name: 'Page 1' })).toHaveAttribute('aria-current', 'page');
    await pages.getByRole('button', { name: 'Next page' }).click();
    await expect(pages.getByRole('button', { name: 'Page 2' })).toHaveAttribute('aria-current', 'page');
    await expect(page.getByRole('cell', { name: /^player-/ }).first()).toBeVisible();
  });
}
