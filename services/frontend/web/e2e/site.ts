// What the tests know about the site: its pages (from the server's PAGES table, through the
// OpenAPI document) and who logs in.
import { expect, type BrowserContext, type Page } from '@playwright/test';
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
const USERS = {
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

/** Logs `context` in as `who`. The API call carries no Origin (it isn't a page's), so the
 * server's same-origin rule lets it through over plain HTTP too. */
export async function logIn(context: BrowserContext, who: Who): Promise<void> {
  if (who === 'anonymous') return;
  const res = await context.request.post('/api/session/login', { data: USERS[who] });
  expect(res.status(), await res.text()).toBe(200);
}

/** No serious or critical accessibility problems on the page as it is now. */
export async function expectAccessible(page: Page): Promise<void> {
  const { violations } = await new AxeBuilder({ page }).analyze();
  const bad = violations.filter((v) => v.impact === 'serious' || v.impact === 'critical');
  expect(bad.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(' ')).join(', ')}`)).toEqual([]);
}
