// What the tests know about the site: its pages (from the server's PAGES table, through the
// OpenAPI document) and who logs in.
import { expect, test as base, type Browser, type BrowserContext, type Page } from '@playwright/test';
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

/** Staging is behind Miles's login (oauth2-proxy and authentik). The staging journeys (release.yml) get past it with a
 * GitHub Actions token for this audience, which the gateway checks is that workflow's
 * (homelab: istio/manifests/login-admin-pages.yaml). A token lasts five minutes, so a fresh one
 * is asked for shortly before the last one runs out. */
let token: { value: string; exp: number } | undefined;
async function stagingToken(): Promise<string> {
  if (token && token.exp - 60 > Date.now() / 1000) return token.value;
  const url = process.env.ACTIONS_ID_TOKEN_REQUEST_URL;
  const bearer = process.env.ACTIONS_ID_TOKEN_REQUEST_TOKEN;
  if (!url || !bearer) throw new Error('Against staging the tests need a GitHub Actions token: they run in the release workflow');
  const audience = new URL(process.env.E2E_BASE_URL!).host;
  const res = await fetch(`${url}&audience=${encodeURIComponent(audience)}`, { headers: { Authorization: `Bearer ${bearer}` } });
  if (!res.ok) throw new Error(`GitHub gave no token: ${res.status} ${await res.text()}`);
  const { value } = (await res.json()) as { value: string };
  const { exp } = JSON.parse(Buffer.from(value.split('.')[1], 'base64url').toString()) as { exp: number };
  token = { value, exp };
  return value;
}

const TOKEN = 'x-staging-journeys-token';

/** The only hosts besides staging a page may reach, though the token goes along: Cloudflare's
 * analytics beacon, which Cloudflare puts in every page it serves (it already sees every request,
 * token included), and GitHub, where the login with GitHub link goes (it issued the token). */
const ELSEWHERE = new Set(['static.cloudflareinsights.com', 'github.com']);

/** oauth2-proxy's redirect to the login for a staging request it turned away: its `state` is
 * `<nonce>:<the URL turned away>`. Chromium doesn't tie a closing page's keepalive beacon to the
 * redirect it got, so `redirectedFrom` alone misses that one. */
function turnedAway(url: URL, origin: string): boolean {
  const state = url.searchParams.get('state');
  return !!state && state.slice(state.indexOf(':') + 1).startsWith(`${origin}/`);
}

/** Against staging, gives `context` the token, and says which requests reached a host other than
 * staging and those above. A redirect staging itself answered with doesn't count: that is staging's
 * login gate turning away a request without the token (the tracing beacon a closing page sends
 * gets no extra headers), not the page reaching another host. */
async function admit(context: BrowserContext): Promise<string[]> {
  const away: string[] = [];
  if (!STAGING) return away;
  await context.setExtraHTTPHeaders({ [TOKEN]: await stagingToken() });
  const origin = new URL(process.env.E2E_BASE_URL!).origin;
  context.on('request', (r) => {
    const url = new URL(r.url());
    const from = r.redirectedFrom();
    if ((from && new URL(from.url()).origin === origin) || turnedAway(url, origin)) return;
    if (url.protocol.startsWith('http') && url.origin !== origin && !ELSEWHERE.has(url.host)) away.push(r.url());
  });
  return away;
}

/** Playwright's `test`, with every test's browser context and API requests admitted to staging. */
export const test = base.extend({
  // Used by both `context` and `request`.
  extraHTTPHeaders: async ({ extraHTTPHeaders }, use) => {
    await use(STAGING ? { ...extraHTTPHeaders, [TOKEN]: await stagingToken() } : extraHTTPHeaders);
  },
  context: async ({ context }, use) => {
    const away = await admit(context);
    await use(context);
    expect(away, 'requests that left staging').toEqual([]);
  },
});

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
  // The placeholders' text is transparent on purpose (it only sizes them), so it has no contrast
  // to check. The stand-ins answer before axe looks; staging's real services not always.
  const { violations } = await new AxeBuilder({ page }).exclude('.text-transparent').analyze();
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
  const away = await admit(context);
  await logIn(context, who, fresh);
  const page = await context.newPage();
  expect(page.viewportSize()).toEqual(size);
  await page.goto(path);
  await page.locator(`main ${ready ?? '[role="status"]'}`).first().waitFor();
  await page.evaluate(() => document.fonts.ready);
  const box = await page.locator(measure).first().boundingBox();
  await context.close();
  expect(away, 'requests that left staging').toEqual([]);
  return box!.height;
}
