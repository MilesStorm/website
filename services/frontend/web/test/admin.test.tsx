import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { Schemas } from '@/api/client';
import { ann, at, json, splice } from './fixtures';

beforeEach(() => {
  vi.resetModules();
  document.head.innerHTML = '';
});
afterEach(cleanup);

const ROLES: Schemas['AdminRoleRef'][] = [
  { id: 1, name: 'admin' },
  { id: 2, name: 'arcane_user' },
  { id: 3, name: 'llama_crew' },
];
const PERMISSIONS: Schemas['AdminPermission'][] = [
  { id: 3, name: 'arcane' },
  { id: 9, name: 'brand_new' },
];
const invite = (over: Partial<Schemas['AdminInvite']> = {}): Schemas['AdminInvite'] => ({
  id: 4,
  role_id: 2,
  role: 'arcane_user',
  note: 'Sam',
  created_by: 'ann',
  created_at: '2026-10-19T00:00:00Z',
  expires_at: '2026-10-26T00:00:00Z',
  max_uses: 1,
  uses: 0,
  revoked: false,
  live: true,
  joined: [],
  link: null,
  ...over,
});

type Handler = (url: URL, body: unknown) => Response | undefined;

/** The admin API as a stand-in: the pickers' lists, then `handle` for the rest. Every request is
 * kept as `METHOD path?query` with its body. */
function serve(handle: Handler) {
  const calls: { call: string; body: unknown }[] = [];
  vi.stubGlobal('location', at('/admin').location);
  vi.stubGlobal('fetch', async (request: Request) => {
    const url = new URL(request.url);
    const text = await request.text();
    const body: unknown = text ? JSON.parse(text) : null;
    calls.push({ call: `${request.method} ${url.pathname}${url.search}`, body });
    if (url.pathname === '/api/admin/roles/all') return json(ROLES);
    if (url.pathname === '/api/admin/permissions') return json(PERMISSIONS);
    return handle(url, body) ?? json({ error: 'not_found', message: 'There is no such API.' }, 404);
  });
  return calls;
}

const user: Schemas['AdminUser'] = { id: 5, username: 'ann', email: null, roles: [{ id: 2, name: 'arcane_user' }] };

describe('the users tab', () => {
  it('lists, searches after typing stops, adds and removes roles', async () => {
    const calls = serve((url, _) => {
      if (url.pathname === '/api/admin/users') return json({ items: [user], total: 1 });
      if (url.pathname.startsWith('/api/admin/users/')) return new Response(null, { status: 204 });
    });
    const { AdminPanel } = await import('@/islands/AdminPanel');
    render(<AdminPanel />);
    expect(await screen.findByText('ann')).toBeTruthy();
    expect(screen.getByText('1 user')).toBeTruthy();
    await userEvent.type(screen.getByLabelText('Search users'), 'an');
    await waitFor(() => expect(calls.some((c) => c.call === 'GET /api/admin/users?page=0&limit=25&search=an')).toBe(true));
    // Typing waited: no request per letter.
    expect(calls.some((c) => c.call.endsWith('search=a'))).toBe(false);

    await userEvent.selectOptions(screen.getByLabelText('Role to add to ann'), 'llama_crew');
    await userEvent.click(screen.getByRole('button', { name: 'Add role to ann' }));
    await waitFor(() => expect(calls.some((c) => c.call === 'PUT /api/admin/users/5/roles/3')).toBe(true));
    await userEvent.click(screen.getByRole('button', { name: 'Remove role arcane_user from ann' }));
    await waitFor(() => expect(calls.some((c) => c.call === 'DELETE /api/admin/users/5/roles/2')).toBe(true));
  });

  it("shows the server's refusal of a change", async () => {
    serve((url) => {
      if (url.pathname === '/api/admin/users') return json({ items: [user], total: 1 });
      return json({ error: 'not_found', message: 'That no longer exists. Reload the page.' }, 404);
    });
    const { AdminPanel } = await import('@/islands/AdminPanel');
    render(<AdminPanel />);
    await userEvent.click(await screen.findByRole('button', { name: 'Remove role arcane_user from ann' }));
    expect(await screen.findByText('That no longer exists. Reload the page.')).toBeTruthy();
  });

  it('keeps a page chosen right after loading', async () => {
    const calls = serve((url) => {
      if (url.pathname === '/api/admin/users') return json({ items: [user], total: 60 });
    });
    const { AdminPanel, SEARCH_DELAY_MS } = await import('@/islands/AdminPanel');
    render(<AdminPanel />);
    const pages = await screen.findByRole('navigation', { name: 'Pages' });
    await userEvent.click(within(pages).getByRole('button', { name: 'Page 2' }));
    await new Promise((r) => setTimeout(r, SEARCH_DELAY_MS + 150));
    expect(within(pages).getByRole('button', { name: 'Page 2' }).getAttribute('aria-current')).toBe('page');
    expect(calls.filter((c) => c.call.startsWith('GET /api/admin/users')).map((c) => c.call)).toEqual([
      'GET /api/admin/users?page=0&limit=25&search=',
      'GET /api/admin/users?page=1&limit=25&search=',
    ]);
  });

  it('pages through many users', async () => {
    const calls = serve((url) => {
      if (url.pathname === '/api/admin/users') return json({ items: [user], total: 60 });
    });
    const { AdminPanel } = await import('@/islands/AdminPanel');
    render(<AdminPanel />);
    const pages = await screen.findByRole('navigation', { name: 'Pages' });
    expect(within(pages).getByRole('button', { name: 'Page 1' }).getAttribute('aria-current')).toBe('page');
    await userEvent.click(within(pages).getByRole('button', { name: 'Next page' }));
    await waitFor(() => expect(calls.some((c) => c.call === 'GET /api/admin/users?page=1&limit=25&search=')).toBe(true));
    expect(within(pages).getByRole('button', { name: 'Page 3' })).toBeTruthy();

    // A search from page 2 starts at the first page, in one request.
    const before = calls.length;
    await userEvent.type(screen.getByLabelText('Search users'), 'an');
    await waitFor(() => expect(calls.slice(before).map((c) => c.call)).toEqual(['GET /api/admin/users?page=0&limit=25&search=an']));
    await new Promise((r) => setTimeout(r, 400));
    expect(calls.slice(before).map((c) => c.call)).toEqual(['GET /api/admin/users?page=0&limit=25&search=an']);
  });

  it('asks before taking a role off your own account, and says why', async () => {
    splice(ann);
    const calls = serve((url) => {
      if (url.pathname === '/api/admin/users') return json({ items: [user], total: 1 });
      if (url.pathname.startsWith('/api/admin/users/')) return new Response(null, { status: 204 });
    });
    const { AdminPanel, OWN_ROLE } = await import('@/islands/AdminPanel');
    render(<AdminPanel />);
    await userEvent.click(await screen.findByRole('button', { name: 'Remove role arcane_user from ann' }));
    const dialog = await screen.findByRole('alertdialog', { name: 'Remove arcane_user from ann?' });
    expect(within(dialog).getByText(OWN_ROLE)).toBeTruthy();
    await userEvent.click(within(dialog).getByRole('button', { name: 'Keep' }));
    expect(calls.some((c) => c.call.startsWith('DELETE'))).toBe(false);
    await userEvent.click(screen.getByRole('button', { name: 'Remove role arcane_user from ann' }));
    await userEvent.click(await screen.findByRole('button', { name: 'Remove' }));
    await waitFor(() => expect(calls.some((c) => c.call === 'DELETE /api/admin/users/5/roles/2')).toBe(true));
  });
});

describe('the roles tab', () => {
  it("marks a permission this site doesn't know, and grants one", async () => {
    const role: Schemas['AdminRole'] = { id: 2, name: 'arcane_user', permissions: [{ id: 9, name: 'brand_new' }] };
    const calls = serve((url) => {
      if (url.pathname === '/api/admin/users') return json({ items: [], total: 0 });
      if (url.pathname === '/api/admin/roles') return json({ items: [role], total: 1 });
      if (url.pathname.startsWith('/api/admin/roles/')) return new Response(null, { status: 204 });
    });
    const { AdminPanel } = await import('@/islands/AdminPanel');
    render(<AdminPanel />);
    await userEvent.click(await screen.findByRole('tab', { name: 'Roles' }));
    expect(await screen.findByText('(unused)')).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Add permission to arcane_user' }));
    await waitFor(() => expect(calls.some((c) => c.call === 'PUT /api/admin/roles/2/permissions/3')).toBe(true));
  });
});

describe('the roles tab, manage_permissions', () => {
  it('asks before taking it off a role, and says why', async () => {
    const role: Schemas['AdminRole'] = {
      id: 1,
      name: 'admin',
      permissions: [
        { id: 3, name: 'arcane' },
        { id: 4, name: 'manage_permissions' },
      ],
    };
    const calls = serve((url) => {
      if (url.pathname === '/api/admin/users') return json({ items: [], total: 0 });
      if (url.pathname === '/api/admin/roles') return json({ items: [role], total: 1 });
      if (url.pathname.startsWith('/api/admin/roles/')) return new Response(null, { status: 204 });
    });
    const { AdminPanel, MANAGE_PERMISSIONS } = await import('@/islands/AdminPanel');
    render(<AdminPanel />);
    await userEvent.click(await screen.findByRole('tab', { name: 'Roles' }));
    // Another permission goes at once.
    await userEvent.click(await screen.findByRole('button', { name: 'Remove permission arcane from admin' }));
    await waitFor(() => expect(calls.some((c) => c.call === 'DELETE /api/admin/roles/1/permissions/3')).toBe(true));
    await userEvent.click(screen.getByRole('button', { name: 'Remove permission manage_permissions from admin' }));
    const dialog = await screen.findByRole('alertdialog', { name: 'Remove manage_permissions from admin?' });
    expect(within(dialog).getByText(MANAGE_PERMISSIONS)).toBeTruthy();
    expect(calls.some((c) => c.call === 'DELETE /api/admin/roles/1/permissions/4')).toBe(false);
    await userEvent.click(within(dialog).getByRole('button', { name: 'Remove' }));
    await waitFor(() => expect(calls.some((c) => c.call === 'DELETE /api/admin/roles/1/permissions/4')).toBe(true));
  });
});

describe('the invites tab', () => {
  it('makes a link for the default role and shows it once', async () => {
    const calls = serve((url) => {
      if (url.pathname === '/api/admin/users') return json({ items: [], total: 0 });
      if (url.pathname === '/api/admin/invites') {
        return calls.at(-1)?.call.startsWith('POST')
          ? json(invite({ link: 'https://milesstorm.test/invite#code' }))
          : json([invite({ revoked: true, live: false })]);
      }
    });
    const { AdminPanel } = await import('@/islands/AdminPanel');
    render(<AdminPanel />);
    await userEvent.click(await screen.findByRole('tab', { name: 'Invites' }));
    expect(await screen.findByText('Turned off')).toBeTruthy();
    await userEvent.type(screen.getByLabelText("Note (who it's for)"), 'Sam');
    await userEvent.click(screen.getByRole('button', { name: 'Make link' }));
    expect(((await screen.findByLabelText('New invite link')) as HTMLInputElement).value).toBe('https://milesstorm.test/invite#code');
    expect(calls.find((c) => c.call === 'POST /api/admin/invites')?.body).toEqual({ role_id: 2, days: 7, max_uses: 1, note: 'Sam' });
  });

  it('turns a link off after asking, and says when it could not', async () => {
    let refuse = false;
    const calls = serve((url) => {
      if (url.pathname === '/api/admin/users') return json({ items: [], total: 0 });
      if (url.pathname === '/api/admin/invites') return json([invite()]);
      if (url.pathname === '/api/admin/invites/4') {
        return refuse
          ? json({ error: 'not_found', message: 'That invite link no longer exists.' }, 404)
          : new Response(null, { status: 204 });
      }
    });
    const { AdminPanel } = await import('@/islands/AdminPanel');
    render(<AdminPanel />);
    await userEvent.click(await screen.findByRole('tab', { name: 'Invites' }));
    await userEvent.click(await screen.findByRole('button', { name: 'Turn off Sam' }));
    await userEvent.click(await screen.findByRole('button', { name: 'Keep' }));
    expect(calls.some((c) => c.call.startsWith('DELETE'))).toBe(false);
    await userEvent.click(screen.getByRole('button', { name: 'Turn off Sam' }));
    await userEvent.click(await screen.findByRole('button', { name: 'Yes, turn off' }));
    await waitFor(() => expect(calls.some((c) => c.call === 'DELETE /api/admin/invites/4')).toBe(true));
    refuse = true;
    await userEvent.click(screen.getByRole('button', { name: 'Turn off Sam' }));
    await userEvent.click(await screen.findByRole('button', { name: 'Yes, turn off' }));
    expect(await screen.findByText('Not turned off: That invite link no longer exists.')).toBeTruthy();
  });
});

describe('the admin rules', () => {
  it('give invites their status, the pager its buttons, and check a new link', async () => {
    const { inviteStatus, newInvite, pageButtons } = await import('@/lib/admin');
    expect(inviteStatus(invite())).toBe('Active');
    expect(inviteStatus(invite({ live: false, uses: 1 }))).toBe('Used up');
    expect(inviteStatus(invite({ live: false, max_uses: null }))).toBe('Expired');
    expect(inviteStatus(invite({ revoked: true, live: false, uses: 1 }))).toBe('Turned off');
    expect(pageButtons(0, 25)).toEqual([]);
    expect(pageButtons(5, 250)).toEqual([0, 1, null, 3, 4, 5, 6, 7, 8, 9]);
    expect(pageButtons(0, 500)).toEqual([0, 1, 2, null, 18, 19]);
    expect(newInvite({ roleId: 2, days: 'x', maxUses: '', note: '' })).toEqual({ ok: false, message: 'Days must be a number.' });
    expect(newInvite({ roleId: 0, days: '7', maxUses: '', note: '' })).toEqual({ ok: false, message: 'Choose which role the link gives.' });
    expect(newInvite({ roleId: 2, days: '7', maxUses: '', note: ' ' })).toEqual({
      ok: true,
      invite: { role_id: 2, days: 7, max_uses: null, note: null },
    });
  });
});
