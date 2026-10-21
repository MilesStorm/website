// What the tests know about the site: its pages (from the server's PAGES table, through the
// OpenAPI document) and who logs in.
import { expect, test, type Browser, type BrowserContext, type Page } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';
import spec from '../src/api/openapi.json' with { type: 'json' };
import type { components } from '../src/api/schema';

export type Access = 'public' | 'guest_only' | 'logged_in' | { perm: components['schemas']['KnownPermission'] };
export const PAGES = (spec as unknown as { 'x-pages': { path: components['schemas']['PagePath']; access: Access }[] })[
  'x-pages'
];

/** Against staging (real auth, its test accounts) rather than the image with stand-ins. */
export const STAGING = !!process.env.E2E_BASE_URL;

/** Logged out, logged in without permissions, with every permission, and logged in while auth
 * can't be asked (only the stand-ins can arrange that). */
export type Who = 'anonymous' | 'plain' | 'all' | 'down';

/** Test accounts: the stand-ins' by default, staging's from the environment (secrets). */
export const USERS = {
  plain: {
    username: process.env.E2E_PLAIN_USERNAME ?? 'tester-none',
    password: process.env.E2E_PLAIN_PASSWORD ?? 'right',
  },
  all: {
    username: process.env.E2E_ALL_USERNAME ?? 'tester-all',
    password: process.env.E2E_ALL_PASSWORD ?? 'right',
  },
  down: { username: 'tester-down', password: 'right' },
};

/** Logs `context` in as `who` and says the username; with `fresh`, as a new stand-in account of
 * that kind. The API call carries no Origin (it isn't a page's), so the server's same-origin rule
 * lets it through. */
export async function logIn(context: BrowserContext, who: Who, fresh: boolean | string = false): Promise<string> {
  if (who === 'anonymous') return '';
  const user = { ...USERS[who] };
  // A stand-in account of its own (fake-upstreams.mjs), for a test that changes it.
  // A string starts the suffix: a stand-in reads some (`busy`, fake-upstreams.mjs).
  if (fresh) user.username += `-${typeof fresh === 'string' ? fresh : ''}${Math.random().toString(36).slice(2, 10)}`;
  const res = await context.request.post('/api/session/login', { data: user });
  expect(res.status(), await res.text()).toBe(200);
  return user.username;
}

/** No serious or critical accessibility problems on the page as it is now. */
export async function expectAccessible(page: Page): Promise<void> {
  const { violations } = await new AxeBuilder({ page }).analyze();
  const bad = violations.filter((v) => v.impact === 'serious' || v.impact === 'critical');
  expect(bad.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(' ')).join(', ')}`)).toEqual([]);
}

/** The height of `measure` on `path`, as `who` (`fresh` as in logIn), in `viewport` (a test's own
 * fixture; the project's when not given): with scripts off, holding
 * the placeholder; with them on, once `ready` (a locator in `main`) shows. Equal: nothing moves
 * when the page arrives. */
export async function shownHeight(
  browser: Browser,
  path: string,
  measure: string,
  ready: string | null,
  who: Who = 'anonymous',
  fresh: boolean | string = false,
  viewport?: { width: number; height: number } | null,
): Promise<number> {
  const { use } = test.info().project;
  const size = viewport ?? use.viewport;
  const context = await browser.newContext({ ...use, viewport: size, javaScriptEnabled: ready !== null });
  await logIn(context, who, fresh);
  const page = await context.newPage();
  expect(page.viewportSize()).toEqual(size);
  await page.goto(path);
  await page.locator(`main ${ready ?? '[role="status"]'}`).first().waitFor();
  await page.evaluate(() => document.fonts.ready);
  const box = await page.locator(measure).first().boundingBox();
  await context.close();
  return box!.height;
}
