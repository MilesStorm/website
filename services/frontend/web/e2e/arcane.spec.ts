// The Arcane dice reader with Chromium's fake camera, against the stand-in ai_pipeline
// (fake-upstreams.mjs, answering with the contracts/arcane messages). Not against staging: a
// camera there sends frames to the real ai_pipeline, and each roll's picture is kept.
import { expect, test, type Page } from '@playwright/test';
import { STAGING, expectAccessible, logIn } from './site';

test.use({
  launchOptions: { args: ['--use-fake-device-for-media-stream', '--use-fake-ui-for-media-stream'] },
  permissions: ['camera'],
});

/** The camera page, connected and reading frames. */
async function reading(page: Page) {
  await page.goto('/arcane');
  await expect(page.getByRole('status', { name: 'Connection' })).toHaveText('Connected');
  // The badge shows the dice server's reading time once frames are answered.
  await expect(page.getByText('42ms', { exact: true })).toBeVisible();
}

if (!STAGING) {
  test('frames go to the dice server, the answers are drawn, a roll is shown; in light and dark', async ({ context, page }) => {
    await logIn(context, 'all');
    for (const colorScheme of ['light', 'dark'] as const) {
      await page.emulateMedia({ colorScheme });
      await reading(page);
      const roll = page.getByRole('main').locator('[data-slot="card"]', { hasText: 'Last roll' });
      await expect(roll.locator('[data-slot="card-title"]')).toHaveText('16');
      // The overlay has the detection's box on it.
      const drawn = await page.locator('canvas[aria-hidden]').evaluate((c: HTMLCanvasElement) => {
        const data = c.getContext('2d')!.getImageData(0, 0, c.width, c.height).data;
        return data.some((v, i) => i % 4 === 3 && v > 0);
      });
      expect(drawn).toBe(true);
      await page.getByRole('button', { name: 'debug' }).click();
      await expect(page.getByText(/det\[0\] dice=16/)).toBeVisible();
      await expectAccessible(page);
    }
  });

  test('a settled roll reaches the live rolls stream, and the latest one can be flagged', async ({ context, page }) => {
    await logIn(context, 'all', true);
    const watcher = await context.newPage();
    await watcher.goto('/account-deleted');
    // The extension's view: the rolls stream (server-sent events) and who is logged in.
    const rolled = watcher.evaluate(
      () =>
        new Promise<{ roll_id: string; total: number | null }>((resolve) => {
          const events = new EventSource('/api/arcane/rolls');
          events.addEventListener('roll', (e) => {
            events.close();
            resolve(JSON.parse((e as MessageEvent).data));
          });
        }),
    );
    await reading(page);
    const roll = await rolled;
    expect(roll.roll_id).toMatch(/^[0-9a-f]+-[0-9a-f]{8}$/);
    expect(roll.total).toBe(16);
    const me = await watcher.evaluate(async () => (await fetch('/api/arcane/me')).json());
    expect(me).toMatchObject({ logged_in: true, has_arcane: true });

    // Flagging a roll as read wrong, as the extension does: the latest one, with a correction.
    await page.close();
    const flagged = await watcher.evaluate(async (rollId) => {
      const res = await fetch('/api/arcane/flag', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ roll_id: rollId, values: ['15'] }),
      });
      return { status: res.status, body: await res.json() };
    }, roll.roll_id);
    expect(flagged).toEqual({ status: 200, body: { ok: true } });
  });

  test('a camera the browser refuses says so, and nothing is sent', async ({ browser }) => {
    const context = await browser.newContext({ baseURL: test.info().project.use.baseURL, ignoreHTTPSErrors: true, permissions: [] });
    const page = await context.newPage();
    await page.addInitScript(() => {
      navigator.mediaDevices.getUserMedia = () => Promise.reject(new DOMException('Permission denied', 'NotAllowedError'));
    });
    await logIn(context, 'all');
    const sockets: string[] = [];
    page.on('websocket', (ws) => sockets.push(ws.url()));
    await page.goto('/arcane');
    await expect(page.getByRole('status', { name: 'Connection' })).toHaveText('camera permission denied');
    expect(sockets).toEqual([]);
    await context.close();
  });
}
