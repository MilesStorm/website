import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { ann, json, splice } from './fixtures';

beforeEach(() => {
  vi.resetModules();
  document.head.innerHTML = '';
  localStorage.clear();
  document.documentElement.className = 'dark';
});
afterEach(cleanup);

describe('the user menu', () => {
  it('shows the spliced viewer and logs out with a new page', async () => {
    splice(ann);
    const assign = vi.fn();
    vi.stubGlobal('location', { origin: 'http://localhost', pathname: '/', assign });
    const fetch = vi.fn(async (_: Request) => new Response(null, { status: 204 }));
    vi.stubGlobal('fetch', fetch);
    const { UserMenu } = await import('@/islands/UserMenu');
    render(<UserMenu />);
    const trigger = await screen.findByRole('button', { name: 'Account: Ann </script> Example' });
    await userEvent.click(trigger);
    await userEvent.click(await screen.findByRole('menuitem', { name: 'Log out' }));
    await waitFor(() => expect(assign).toHaveBeenCalledWith('/'));
    expect(fetch.mock.calls[0][0].method).toBe('DELETE');
  });
});

describe('the theme toggle', () => {
  it('saves the choice and applies it', async () => {
    const { ModeToggle } = await import('@/islands/ModeToggle');
    render(<ModeToggle />);
    await userEvent.click(screen.getByRole('button', { name: 'Theme' }));
    await userEvent.click(await screen.findByRole('menuitemradio', { name: 'Light' }));
    expect(localStorage.getItem('ms.theme')).toBe('light');
    expect(document.documentElement.classList.contains('dark')).toBe(false);
  });
});

describe('the cookie notice', () => {
  it('stays away once accepted', async () => {
    const { CookieConsent } = await import('@/islands/CookieConsent');
    const { unmount } = render(<CookieConsent />);
    await userEvent.click(await screen.findByRole('button', { name: 'Accept' }));
    expect(screen.queryByRole('region', { name: 'Cookies' })).toBeNull();
    expect(localStorage.getItem('ms.cookie-consent')).toBe('accepted');
    unmount();
    render(<CookieConsent />);
    expect(screen.queryByRole('region', { name: 'Cookies' })).toBeNull();
  });
});

describe('the logged-in start page', () => {
  it('greets the viewer and lists their tools', async () => {
    vi.stubGlobal('fetch', async () => json(ann));
    const { HomeCards } = await import('@/islands/HomeCards');
    render(<HomeCards />);
    expect(await screen.findByRole('heading', { name: 'Ann </script> Example' })).toBeTruthy();
    expect(screen.getByRole('link', { name: /Arcane dice reader/ }).getAttribute('href')).toBe('/arcane');
  });
});

describe('the countdown', () => {
  it('splits seconds into days, hours, minutes and seconds', async () => {
    const { split } = await import('@/islands/Countdown');
    expect(split(86_400 * 2 + 3600 * 3 + 60 * 4 + 5)).toEqual([2, 3, 4, 5]);
  });
});
