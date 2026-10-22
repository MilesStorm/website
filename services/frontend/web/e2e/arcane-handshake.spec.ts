// The camera WebSocket's door, safe on staging too: a handshake opens for an account with
// `arcane` and is closed at once, before any frame; without the permission it never opens.
import { expect, type Page } from '@playwright/test';
import { logIn, test } from './site';

/** Opens `/ws/arcane` from the page and closes it as soon as it opens: whether it opened. */
function handshake(page: Page): Promise<boolean> {
  return page.evaluate(
    () =>
      new Promise<boolean>((resolve) => {
        const ws = new WebSocket(`${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/ws/arcane`);
        ws.onopen = () => {
          ws.close();
          resolve(true);
        };
        ws.onclose = () => resolve(false);
      }),
  );
}

test('without arcane the camera WebSocket never opens', async ({ context, page }) => {
  await logIn(context, 'plain');
  await page.goto('/account-deleted');
  expect(await handshake(page)).toBe(false);
});

test('with arcane it opens, and is closed before any frame is sent', async ({ context, page }) => {
  await logIn(context, 'all');
  await page.goto('/account-deleted');
  const sent: unknown[] = [];
  page.on('websocket', (ws) => ws.on('framesent', (f) => sent.push(f)));
  expect(await handshake(page)).toBe(true);
  expect(sent).toEqual([]);
});
