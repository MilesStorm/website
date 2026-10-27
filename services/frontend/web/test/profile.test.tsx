import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { Schemas } from '@/api/client';
import { ann, at, json, server, splice } from './fixtures';

beforeEach(() => {
  vi.resetModules();
  document.head.innerHTML = '';
});
afterEach(cleanup);

const account: Schemas['AccountInfo'] = {
  username: 'ann',
  display_name: null,
  shown_name: 'ann',
  picture_version: null,
  picture_url: null,
  pictures_available: true,
  email: 'ann@example.test',
  email_verified: false,
};

function open(path: string, routes: Parameters<typeof server>[0]) {
  const page = at(path);
  const api = server(routes);
  vi.stubGlobal('location', page.location);
  vi.stubGlobal('fetch', api.fetch);
  return { assigned: page.assigned, calls: api.calls };
}

describe('the profile', () => {
  it('saves the display name, and tells the user menu', async () => {
    splice(ann);
    const named = { ...account, display_name: 'Annie', shown_name: 'Annie' };
    const { calls } = open('/profile', {
      'GET /api/account': () => json(account),
      'PUT /api/account/display-name': () => json(named),
      'GET /api/account/sharing': () => json({ available: false, share: false }),
    });
    const seen: string[] = [];
    window.addEventListener('ms:viewer', (e) => seen.push((e as CustomEvent<{ shown_name: string }>).detail.shown_name));
    const { Profile } = await import('@/islands/Profile');
    render(<Profile />);
    const name = await screen.findByLabelText('Display name');
    expect(screen.getByRole('button', { name: 'Save' })).toHaveProperty('disabled', true);
    await userEvent.type(name, 'Annie');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(await screen.findByText('Saved.')).toBeTruthy();
    expect(calls.find((c) => c.route === 'PUT /api/account/display-name')?.body).toEqual({ name: 'Annie' });
    expect(seen).toEqual(['Annie']);
  });

  it('uploads a picture as raw bytes, and refuses one too large before sending it', async () => {
    splice(ann);
    let current = account;
    const sent: { type: string | null; size: number }[] = [];
    const page = at('/profile');
    vi.stubGlobal('location', page.location);
    vi.stubGlobal('fetch', async (request: Request) => {
      const route = `${request.method} ${new URL(request.url).pathname}`;
      if (route === 'POST /api/account/picture') {
        sent.push({ type: request.headers.get('content-type'), size: (await request.arrayBuffer()).byteLength });
        current = { ...account, picture_version: 7, picture_url: '/api/account/picture?u=ann&v=7' };
        return json({ version: 7 });
      }
      if (route === 'GET /api/account/sharing') return json({ available: false, share: false });
      return json(current);
    });
    const { Profile, PICTURE_MAX_BYTES } = await import('@/islands/Profile');
    render(<Profile />);
    const input = (await screen.findByLabelText('Profile picture file')) as HTMLInputElement;
    await userEvent.upload(input, new File([new Uint8Array(PICTURE_MAX_BYTES + 1)], 'big.png', { type: 'image/png' }));
    expect(await screen.findByText('That picture is too large (at most 5 MB).')).toBeTruthy();
    expect(sent).toEqual([]);
    await userEvent.upload(input, new File([new Uint8Array(10)], 'me.png', { type: 'image/png' }));
    expect(await screen.findByText('Picture updated.')).toBeTruthy();
    expect(sent).toEqual([{ type: 'application/octet-stream', size: 10 }]);
    expect(screen.getByAltText('Your profile picture').getAttribute('src')).toBe('/api/account/picture?u=ann&v=7');
    expect(screen.getByRole('button', { name: 'Remove' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Change' })).toBeTruthy();
  });

  it('keeps the appearance and Log out when the account will not load', async () => {
    splice(ann);
    open('/profile', { 'GET /api/account': () => json({ error: 'down' }, 500) });
    const { Profile } = await import('@/islands/Profile');
    render(<Profile />);
    expect(await screen.findByRole('alert')).toBeTruthy();
    expect(screen.getByRole('radiogroup', { name: 'Theme' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Log out' })).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Delete my account…' })).toBeNull();
  });

  it('logs out from the page', async () => {
    splice(ann);
    const { assigned, calls } = open('/profile', {
      'GET /api/account': () => json(account),
      'GET /api/account/sharing': () => json({ available: false, share: false }),
      'DELETE /api/session': () => new Response(null, { status: 204 }),
    });
    const { Profile } = await import('@/islands/Profile');
    render(<Profile />);
    await userEvent.click(await screen.findByRole('button', { name: 'Log out' }));
    await waitFor(() => expect(assigned).toEqual(['/']));
    expect(calls.at(-1)?.route).toBe('DELETE /api/session');
  });

  it('sends a confirmation email for an unconfirmed address', async () => {
    splice(ann);
    open('/profile', {
      'GET /api/account': () => json(account),
      'POST /api/account/email/send-confirmation': () => json({ email: 'ann@example.test' }),
      'GET /api/account/sharing': () => json({ available: false, share: false }),
    });
    const { Profile } = await import('@/islands/Profile');
    render(<Profile />);
    expect(await screen.findByText('Not confirmed')).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Send confirmation email' }));
    expect(await screen.findByText(/Sent\. Open the link in the email to ann@example\.test/)).toBeTruthy();
  });

  it('shows roll sharing only with the arcane permission', async () => {
    splice({ viewer: { ...ann.viewer!, permissions: ['llama'] } });
    const { calls } = open('/profile', { 'GET /api/account': () => json(account) });
    const { Profile } = await import('@/islands/Profile');
    render(<Profile />);
    await screen.findByText('Account');
    expect(screen.queryByText('Help improve dice recognition')).toBeNull();
    expect(calls.map((c) => c.route)).toEqual(['GET /api/account']);
  });

  it('turns sharing on, and deletes what was shared after a second click', async () => {
    splice(ann);
    const { calls } = open('/profile', {
      'GET /api/account': () => json(account),
      'GET /api/account/sharing': () => json({ available: true, share: false }),
      'PUT /api/account/sharing': () => json({ available: true, share: true }),
      'DELETE /api/account/sharing/data': () => json({ deleted: 3 }),
    });
    const { Profile } = await import('@/islands/Profile');
    render(<Profile />);
    const toggle = await screen.findByRole('switch', { name: 'Share pictures of my rolls' });
    await userEvent.click(toggle);
    await waitFor(() => expect(toggle.getAttribute('aria-checked')).toBe('true'));
    expect(calls.find((c) => c.route === 'PUT /api/account/sharing')?.body).toEqual({ share: true });
    // What is shared sits behind a disclosure.
    expect(screen.queryByText('Who sees them')).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: 'Show' }));
    expect(await screen.findByText('Who sees them')).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Delete my shared pictures' }));
    await userEvent.click(screen.getByRole('button', { name: 'Delete everything' }));
    expect(await screen.findByText('Deleted 3 rolls.')).toBeTruthy();
    expect(toggle.getAttribute('aria-checked')).toBe('false');
  });

  it('deletes an account without a confirmed email once the username is typed', async () => {
    splice(ann);
    const { assigned, calls } = open('/profile', {
      'GET /api/account': () => json({ ...account, email: null }),
      'GET /api/account/sharing': () => json({ available: false, share: false }),
      'POST /api/account/deletion/direct': () => json({ username: 'ann', logged_out: true }),
    });
    const { Profile } = await import('@/islands/Profile');
    render(<Profile />);
    await userEvent.click(await screen.findByRole('button', { name: 'Delete my account…' }));
    const remove = screen.getByRole('button', { name: 'Delete permanently' });
    expect(remove).toHaveProperty('disabled', true);
    await userEvent.type(screen.getByLabelText(/Type your username/), 'ann');
    await userEvent.click(remove);
    await waitFor(() => expect(assigned).toEqual(['/account-deleted']));
    expect(calls.at(-1)?.body).toEqual({ confirm: 'ann' });
  });

  it('emails a deletion link to a confirmed address', async () => {
    splice(ann);
    open('/profile', {
      'GET /api/account': () => json({ ...account, email_verified: true }),
      'GET /api/account/sharing': () => json({ available: false, share: false }),
      'POST /api/account/deletion/request': () => json({ email: 'ann@example.test' }),
    });
    const { Profile } = await import('@/islands/Profile');
    render(<Profile />);
    expect(await screen.findByText('Confirmed')).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Delete my account…' }));
    await userEvent.click(screen.getByRole('button', { name: 'Email me the link' }));
    expect(await screen.findByText('Sent to ann@example.test. The link works for 1 hour.')).toBeTruthy();
  });
});

describe('the Ark panel', () => {
  it('shows who is online, runs a command and asks again', async () => {
    let count: number | null = null;
    const { calls } = open('/ark', {
      'GET /api/ark/players': () => json({ count }),
      'POST /api/ark/command': () => {
        count = 0;
        return json({ outcome: 'started' });
      },
    });
    const { ArkPanel } = await import('@/islands/ArkPanel');
    render(<ArkPanel />);
    expect(await screen.findByText('Offline')).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Start' }));
    expect(await screen.findByText('Started.')).toBeTruthy();
    expect(await screen.findByText('Server running, no players')).toBeTruthy();
    expect(calls.map((c) => c.route)).toEqual(['GET /api/ark/players', 'POST /api/ark/command', 'GET /api/ark/players']);
    expect(calls[1]?.body).toEqual({ cmd: 'start' });
  });

  it('asks again at once when the tab is shown again', async () => {
    const { calls } = open('/ark', { 'GET /api/ark/players': () => json({ count: 1 }) });
    const { ArkPanel } = await import('@/islands/ArkPanel');
    render(<ArkPanel />);
    expect(await screen.findByText('Server is active')).toBeTruthy();
    document.dispatchEvent(new Event('visibilitychange'));
    await waitFor(() => expect(calls.length).toBe(2));
  });

  it('asks again every 30 seconds while the page is shown', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const { calls } = open('/ark', { 'GET /api/ark/players': () => json({ count: 2 }) });
    const { ArkPanel, POLL_MS } = await import('@/islands/ArkPanel');
    render(<ArkPanel />);
    expect(await screen.findByText('Server is active')).toBeTruthy();
    await vi.advanceTimersByTimeAsync(POLL_MS);
    expect(calls.length).toBe(2);
    vi.useRealTimers();
  });
});
