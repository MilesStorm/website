// End-to-end tests of the built image (e2e/serve.sh runs it behind HTTPS, with stand-ins for
// auth and SurrealDB), and the same tests against staging (the `staging` workflow sets
// E2E_BASE_URL and the staging test users).
import { defineConfig, devices } from '@playwright/test';

// Against staging the tests log in with real accounts: a trace, screenshot or HTML report would
// record their passwords and session cookies, so none are kept there.
const staging = !!process.env.E2E_BASE_URL;

export default defineConfig({
  testDir: 'e2e',
  // Every test, not every file, is a unit of work: a file's tests spread over the workers, so the
  // run takes as long as its slowest test rather than its largest file (pages.spec.ts, one test
  // per page and viewer). Files whose tests share stand-in state opt out (admin.spec.ts). Not
  // against staging: its journeys share real accounts.
  fullyParallel: !staging,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI && !staging ? [['list'], ['html', { open: 'never' }]] : 'list',
  use: {
    baseURL: process.env.E2E_BASE_URL ?? 'https://localhost:8443',
    // e2e/serve.sh's Caddy signs with its own CA.
    ignoreHTTPSErrors: !staging,
    trace: staging ? 'off' : 'retain-on-failure',
    screenshot: 'off',
    video: 'off',
  },
  projects: [
    { name: 'chromium', use: { ...devices['Desktop Chrome'] }, testIgnore: 'mobile.spec.ts' },
    // Phones, by touch: Android's browser and an iPhone's WebKit.
    { name: 'pixel', use: { ...devices['Pixel 7'] }, testMatch: 'mobile.spec.ts' },
    { name: 'iphone', use: { ...devices['iPhone 14'] }, testMatch: 'mobile.spec.ts' },
  ],
});
