import { afterEach, describe, expect, it, vi } from 'vitest';
import { bffUrl, makeClient } from '@/api/client';
import { json, loggedOut } from './fixtures';

afterEach(() => vi.unstubAllGlobals());

describe('the API client', () => {
  it('looks fetch up when it calls, so the tracing wrapper installed later is used', async () => {
    const client = makeClient();
    const wrapped = vi.fn(async (_: Request) => json(loggedOut));
    vi.stubGlobal('fetch', wrapped);
    const { data } = await client.GET('/api/session');
    expect(data).toEqual(loggedOut);
    expect(wrapped).toHaveBeenCalledOnce();
    expect(new URL(wrapped.mock.calls[0][0].url).pathname).toBe('/api/session');
  });

  it('sends a logged-out user to log in, coming back to a known page', async () => {
    const assign = vi.fn();
    vi.stubGlobal('location', { origin: 'http://localhost', pathname: '/asshole', assign });
    const client = makeClient(async () => json({ error: 'logged_out', message: 'Log in again.' }, 401));
    await client.GET('/api/account');
    expect(assign).toHaveBeenCalledWith('/login?next=%2Fasshole');

    vi.stubGlobal('location', { origin: 'http://localhost', pathname: '/not-a-page', assign });
    await client.GET('/api/account');
    expect(assign).toHaveBeenLastCalledWith('/login');
  });

  it('leaves other 401s to the caller', async () => {
    const assign = vi.fn();
    vi.stubGlobal('location', { origin: 'http://localhost', pathname: '/', assign });
    const client = makeClient(async () => json({ error: 'invalid', message: 'Wrong password.' }, 401));
    const { error } = await client.POST('/api/session/login', { body: { username: 'a', password: 'b' } });
    expect(error?.error).toBe('invalid');
    expect(assign).not.toHaveBeenCalled();
  });

  it('builds server URLs from one base', () => {
    expect(bffUrl('/api/account/picture?v=1')).toBe('/api/account/picture?v=1');
  });
});
