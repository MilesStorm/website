// What a click is named in traces and Faro user actions (assets/trace.js `labelFor`): never a
// person's name, which a control's text or aria-label can carry.
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';
import { CLICKABLE, labelFor } from '../assets/trace.js';
import type { Schemas } from '@/api/client';
import { ann, at, json, splice } from './fixtures';

beforeEach(() => {
  vi.resetModules();
  document.head.innerHTML = '';
});
afterEach(cleanup);

const NAMES = ['ann', 'Ann', 'Example', 'zed-private', 'Sam'];

function labels(): string[] {
  return [...document.querySelectorAll(CLICKABLE)].map((el) => labelFor(el, null, location.href));
}

function expectNoNames(found: string[]) {
  expect(found.length).toBeGreaterThan(0);
  for (const label of found) {
    for (const name of NAMES) expect(label, label).not.toContain(name);
  }
}

it('the account menu is named without the viewer', async () => {
  splice(ann);
  const { UserMenu } = await import('@/islands/UserMenu');
  render(<UserMenu />);
  await screen.findByRole('button', { name: /^Account: / });
  expectNoNames(labels());
});

it("the admin panel's controls are named without the users they act on", async () => {
  const user: Schemas['AdminUser'] = { id: 5, username: 'zed-private', email: null, roles: [{ id: 2, name: 'arcane_user' }] };
  vi.stubGlobal('location', at('/admin').location);
  vi.stubGlobal('fetch', async (request: Request) => {
    const url = new URL(request.url);
    if (url.pathname === '/api/admin/roles/all') return json([{ id: 1, name: 'admin' }, { id: 2, name: 'arcane_user' }]);
    if (url.pathname === '/api/admin/permissions') return json([]);
    if (url.pathname === '/api/admin/users') return json({ items: [user], total: 60 });
    return json({ error: 'not_found', message: 'There is no such API.' }, 404);
  });
  const { AdminPanel } = await import('@/islands/AdminPanel');
  render(<AdminPanel />);
  await screen.findByRole('button', { name: 'Remove role arcane_user from zed-private' });
  await screen.findByRole('navigation', { name: 'Pages' });
  const found = labels();
  expectNoNames(found);
  expect(found).toEqual(expect.arrayContaining(['admin remove role', 'admin add role', 'admin page next']));
});
