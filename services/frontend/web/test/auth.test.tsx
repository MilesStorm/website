import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { Schemas } from '@/api/client';
import { at, json, server } from './fixtures';

beforeEach(() => {
  vi.resetModules();
  document.head.innerHTML = '';
  delete document.documentElement.dataset.session;
});
afterEach(cleanup);

/** Opens the page at `path` with `routes` as the server. */
function open(path: string, routes: Parameters<typeof server>[0]) {
  const page = at(path);
  const api = server(routes);
  vi.stubGlobal('location', page.location);
  vi.stubGlobal('fetch', api.fetch);
  return { assigned: page.assigned, calls: api.calls };
}

const nextPage = (page: Schemas['NextPage']) => () => json(page);
const error = (body: Schemas['ApiError'], status: number) => () => json(body, status);

describe('logging in', () => {
  it('lands on the page it was opened for, unless an invite waits', async () => {
    const { assigned, calls } = open('/login?next=%2Fasshole', {
      'POST /api/session/login': () => json({ username: 'ann' }),
      'POST /api/session/after-login': nextPage({ path: '/invite', fragment: 'join' }),
    });
    const { LoginForm } = await import('@/islands/LoginForm');
    render(<LoginForm />);
    await userEvent.type(screen.getByLabelText('Username'), 'ann');
    await userEvent.type(screen.getByLabelText('Password'), 'secret');
    await userEvent.click(screen.getByRole('button', { name: 'Log in' }));
    await waitFor(() => expect(assigned).toEqual(['/invite#join']));
    expect(calls.map((c) => c.body)).toEqual([{ username: 'ann', password: 'secret' }, { next: '/asshole' }]);
  });

  it('carries only a page of the site as next, to OAuth too', async () => {
    const { calls } = open('/login?next=%2F%2Fevil.com', {
      'POST /api/session/login': () => json({ username: 'ann' }),
      'POST /api/session/after-login': nextPage({ path: '/', fragment: null }),
    });
    const { LoginForm } = await import('@/islands/LoginForm');
    render(<LoginForm />);
    expect(screen.getByRole('link', { name: /Log in with GitHub/ }).getAttribute('href')).toBe('/oauth/start/github');
    await userEvent.type(screen.getByLabelText('Username'), 'ann');
    await userEvent.type(screen.getByLabelText('Password'), 'secret');
    await userEvent.click(screen.getByRole('button', { name: 'Log in' }));
    await waitFor(() => expect(calls[1]?.body).toEqual({ next: null }));
    cleanup();

    open('/login?next=%2Fasshole', {});
    render(<LoginForm />);
    expect(screen.getByRole('link', { name: /Log in with Google/ }).getAttribute('href')).toBe(
      '/oauth/start/google?next=%2Fasshole',
    );
    expect(screen.getByRole('link', { name: 'Sign up' }).getAttribute('href')).toBe('/register?next=%2Fasshole');
  });

  it("shows the server's message, a missing field, and why OAuth came back", async () => {
    const { assigned } = open('/login?error=email_exists', {
      'POST /api/session/login': error({ error: 'invalid', message: 'Wrong username or password.' }, 401),
    });
    const { LoginForm } = await import('@/islands/LoginForm');
    render(<LoginForm />);
    expect(screen.getByRole('alert').textContent).toMatch(/An account with this email already exists/);
    await userEvent.click(screen.getByRole('button', { name: 'Log in' }));
    const username = screen.getByLabelText('Username');
    expect(username.getAttribute('aria-invalid')).toBe('true');
    const described = document.getElementById(username.getAttribute('aria-describedby')!);
    expect(described?.textContent).toBe('Enter your username.');
    await userEvent.type(username, 'ann');
    await userEvent.type(screen.getByLabelText('Password'), 'wrong');
    await userEvent.click(screen.getByRole('button', { name: 'Log in' }));
    expect(await screen.findByText('Wrong username or password.')).toBeTruthy();
    expect(assigned).toEqual([]);
  });
});

describe('landing after a login', () => {
  it('goes to next when the server gives no answer', async () => {
    const page = at('/login?next=%2Fasshole');
    vi.stubGlobal('location', page.location);
    vi.stubGlobal('fetch', async () => {
      throw new TypeError('Failed to fetch');
    });
    const { landAfterLogin } = await import('@/lib/login');
    await landAfterLogin('/asshole');
    await landAfterLogin(null);
    expect(page.assigned).toEqual(['/asshole', '/']);
  });
});

describe('signing up', () => {
  it('marks a taken email on its field, and lands like a login', async () => {
    let taken = true;
    const { assigned } = open('/register?next=%2Fasshole', {
      'POST /api/session/register': () =>
        taken ? json({ error: 'conflict', message: 'Email already in use' }, 409) : json({ username: 'ann' }),
      'POST /api/session/after-login': nextPage({ path: '/asshole', fragment: null }),
    });
    const { RegisterForm } = await import('@/islands/RegisterForm');
    render(<RegisterForm />);
    await userEvent.type(screen.getByLabelText('Email'), 'ann@example.test');
    await userEvent.type(screen.getByLabelText('Username'), 'ann');
    await userEvent.type(screen.getByLabelText('Password'), 'short');
    await userEvent.click(screen.getByRole('button', { name: 'Sign up with email' }));
    expect(await screen.findByText('Use at least 8 characters.')).toBeTruthy();
    await userEvent.type(screen.getByLabelText('Password'), ' and longer');
    await userEvent.click(screen.getByRole('button', { name: 'Sign up with email' }));
    expect(await screen.findByText('That email is already in use.')).toBeTruthy();
    expect(screen.getByLabelText('Email').getAttribute('aria-invalid')).toBe('true');
    taken = false;
    await userEvent.click(screen.getByRole('button', { name: 'Sign up with email' }));
    await waitFor(() => expect(assigned).toEqual(['/asshole']));
  });
});

describe('the emailed links', () => {
  it('a reset checks the two passwords, and shows a logged-out navbar when it logged this browser out', async () => {
    document.documentElement.dataset.session = 'in';
    const { calls } = open('/reset-password#good', {
      'POST /api/password/reset': () => json({ username: 'ann', logged_out: true }),
    });
    const { ResetForm } = await import('@/islands/ResetForm');
    render(<ResetForm />);
    await userEvent.type(screen.getByLabelText('New password'), 'long enough');
    await userEvent.type(screen.getByLabelText('Type it again'), 'long enoug');
    await userEvent.click(screen.getByRole('button', { name: 'Change password' }));
    expect(await screen.findByText('The two passwords are different.')).toBeTruthy();
    await userEvent.type(screen.getByLabelText('Type it again'), 'h');
    await userEvent.click(screen.getByRole('button', { name: 'Change password' }));
    expect(await screen.findByText(/logged out everywhere/)).toBeTruthy();
    expect(calls[0]?.body).toEqual({ code: 'good', password: 'long enough' });
    expect(document.documentElement.dataset.session).toBe('out');
  });

  it('an expired reset link offers a new one; a cut-off link says so', async () => {
    open('/reset-password#old', {
      'POST /api/password/reset': error({ error: 'gone', message: 'This link has expired or was already used. Ask for a new one.' }, 404),
    });
    const { ResetForm } = await import('@/islands/ResetForm');
    render(<ResetForm />);
    await userEvent.type(screen.getByLabelText('New password'), 'long enough');
    await userEvent.type(screen.getByLabelText('Type it again'), 'long enough');
    await userEvent.click(screen.getByRole('button', { name: 'Change password' }));
    expect(await screen.findByRole('link', { name: 'Get a new link' })).toBeTruthy();
    cleanup();

    open('/reset-password', {});
    render(<ResetForm />);
    expect(screen.getByRole('alert').textContent).toMatch(/This link is incomplete/);
  });

  it('opening a confirm link confirms, with the code in the body', async () => {
    const { calls } = open('/verify-email#good', { 'POST /api/email/confirm': () => json({ username: 'ann' }) });
    const { VerifyEmail } = await import('@/islands/VerifyEmail');
    render(<VerifyEmail />);
    expect(await screen.findByText(/is\s+confirmed/)).toBeTruthy();
    expect(calls).toEqual([{ route: 'POST /api/email/confirm', body: { code: 'good' } }]);
  });

  it('a delete link says whose account, and deletes only on the button', async () => {
    const { assigned, calls } = open('/delete-account#good', {
      'POST /api/deletion/check': () => json({ username: 'ann' }),
      'POST /api/deletion/confirm': () => json({ username: 'ann', logged_out: false }),
    });
    const { DeleteAccount } = await import('@/islands/DeleteAccount');
    render(<DeleteAccount />);
    expect(await screen.findByText('ann')).toBeTruthy();
    expect(calls.map((c) => c.route)).toEqual(['POST /api/deletion/check']);
    await userEvent.click(screen.getByRole('button', { name: 'Delete my account' }));
    await waitFor(() => expect(assigned).toEqual(['/account-deleted']));
  });
});

describe('an invite', () => {
  it('joins when logged in and points at the new page', async () => {
    open('/invite#join', {
      'POST /api/invites/open': () =>
        json({ kind: 'joined', role: 'arcane_user', group_label: 'the Arcane dice test group', next: '/arcane' }),
      'GET /api/session': () => json({ viewer: null }),
    });
    const { InviteOpen } = await import('@/islands/InviteOpen');
    render(<InviteOpen />);
    expect((await screen.findByRole('link', { name: 'Open Arcane dice' })).getAttribute('href')).toBe('/arcane');
  });

  it('logged out, offers signing up or logging in', async () => {
    open('/invite#join', {
      'POST /api/invites/open': () => json({ kind: 'needs_account', role: 'x', group_label: 'the “x” group' }),
    });
    const { InviteOpen } = await import('@/islands/InviteOpen');
    render(<InviteOpen />);
    expect((await screen.findByRole('link', { name: 'Sign up' })).getAttribute('href')).toBe('/register');
  });
});

describe('error messages', () => {
  it("are the server's, else the code's, else that nothing answered", async () => {
    const { attempt, errorMessage, NO_ANSWER } = await import('@/lib/errors');
    expect(errorMessage({ error: 'rate_limited', message: '' })).toBe('Too many tries. Wait a moment and try again.');
    expect(errorMessage('<html>Bad gateway</html>')).toBe(NO_ANSWER);
    const thrown = await attempt(Promise.reject(new TypeError('Failed to fetch')));
    expect(thrown).toEqual({ ok: false, message: NO_ANSWER, code: null });
  });
});
