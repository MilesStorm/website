import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { Schemas } from '@/api/client';
import { at, json, server } from './fixtures';

beforeEach(() => vi.resetModules());
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

const NOW = 10_000;

function status(over: Partial<Schemas['EnshroudedStatus']> = {}): Schemas['EnshroudedStatus'] {
  return {
    state: 'Online',
    players: { online: 2, max: 16, sessions: [300, 7500] },
    version: '1269579',
    uptime_secs: 90_000,
    latest_build_at: 1_000,
    update_pending: true,
    update_queued: false,
    can_control: true,
    recent: [
      { name: 'Ada', action: 'update_now', at: 9_000 },
      { name: null, action: 'restart', at: 2_000 },
      { name: 'Bo', action: 'reinstall', at: 9_500 },
    ],
    next_command_at: null,
    ...over,
  };
}

function open(routes: Parameters<typeof server>[0]) {
  vi.useFakeTimers({ shouldAdvanceTime: true, now: NOW * 1000 });
  const page = at('/enshrouded');
  const api = server(routes);
  vi.stubGlobal('location', page.location);
  vi.stubGlobal('fetch', api.fetch);
  return api.calls;
}

describe('the Enshrouded panel', () => {
  it('shows the server, its players and who did what', async () => {
    open({ 'GET /api/enshrouded': () => json(status()) });
    const { EnshroudedPanel } = await import('@/islands/EnshroudedPanel');
    render(<EnshroudedPanel />);
    expect(await screen.findByText('Online', { exact: true })).toBeTruthy();
    for (const text of [
      'Up for 1 d 1 h',
      'Build 1269579',
      'A new build came out 2 h 30 min ago.',
      'on for 5 min',
      'on for 2 h 5 min',
      'A deleted account',
      '16 min ago',
    ]) {
      expect(await screen.findByText(text, { exact: false })).toBeTruthy();
    }
    expect(screen.getByText('Ada').parentElement?.textContent).toBe('Ada force-updated the server, disconnecting players');
    // A command name this site doesn't know reads as a restart.
    expect(screen.getByText('Bo').parentElement?.textContent).toBe('Bo restarted the server');
    expect(screen.getByRole('button', { name: 'Restart' })).toHaveProperty('disabled', false);
  });

  it('asks before a restart, warns about players, sends the typed command', async () => {
    const calls = open({
      'GET /api/enshrouded': () => json(status()),
      'POST /api/enshrouded/command': () => new Response(null, { status: 204 }),
    });
    const { EnshroudedPanel } = await import('@/islands/EnshroudedPanel');
    render(<EnshroudedPanel />);
    await userEvent.click(await screen.findByRole('button', { name: 'Restart' }));
    expect(screen.getByText('2 players are online. They will be disconnected.')).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Yes, restart' }));
    expect(await screen.findByText('Restart sent. The status above follows along.')).toBeTruthy();
    expect(calls.find((c) => c.route === 'POST /api/enshrouded/command')?.body).toEqual({ cmd: 'restart' });
    // And asked again straight after.
    expect(calls.filter((c) => c.route === 'GET /api/enshrouded').length).toBe(2);
  });

  it('with players on, Force update waits for them unless asked not to', async () => {
    const calls = open({
      'GET /api/enshrouded': () => json(status()),
      'POST /api/enshrouded/command': () => new Response(null, { status: 204 }),
    });
    const { EnshroudedPanel } = await import('@/islands/EnshroudedPanel');
    render(<EnshroudedPanel />);
    await userEvent.click(await screen.findByRole('button', { name: 'Force update' }));
    expect(screen.getByText(/Force update waits until everyone has left/)).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Update now anyway' }));
    expect(await screen.findByText('Update started. The status above follows along.')).toBeTruthy();
    expect(calls.find((c) => c.route === 'POST /api/enshrouded/command')?.body).toEqual({ cmd: 'update_now' });
  });

  it('with nobody on, Force update just asks', async () => {
    open({ 'GET /api/enshrouded': () => json(status({ players: { online: 0, max: 16, sessions: [] } })) });
    const { EnshroudedPanel } = await import('@/islands/EnshroudedPanel');
    render(<EnshroudedPanel />);
    await userEvent.click(await screen.findByRole('button', { name: 'Force update' }));
    expect(screen.getByRole('button', { name: 'Yes, update' })).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Update now anyway' })).toBeNull();
  });

  it("shows auth's refusal, such as a restart already under way (409)", async () => {
    open({
      'GET /api/enshrouded': () => json(status()),
      'POST /api/enshrouded/command': () => json({ error: 'conflict', message: 'A restart or update is already under way.' }, 409),
    });
    const { EnshroudedPanel } = await import('@/islands/EnshroudedPanel');
    render(<EnshroudedPanel />);
    await userEvent.click(await screen.findByRole('button', { name: 'Restart' }));
    await userEvent.click(screen.getByRole('button', { name: 'Yes, restart' }));
    expect(await screen.findByText('A restart or update is already under way.')).toBeTruthy();
  });

  it('waits its turn after a command, and while one is under way', async () => {
    open({ 'GET /api/enshrouded': () => json(status({ next_command_at: NOW + 45 })) });
    const { EnshroudedPanel } = await import('@/islands/EnshroudedPanel');
    render(<EnshroudedPanel />);
    expect(await screen.findByText(/the buttons come back in 4\d s/)).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Restart' })).toHaveProperty('disabled', true);
    expect(screen.getByRole('button', { name: 'Force update' })).toHaveProperty('disabled', true);
    cleanup();

    open({ 'GET /api/enshrouded': () => json(status({ state: 'Updating' })) });
    render(<EnshroudedPanel />);
    expect(await screen.findByText(/Downloading the latest build/)).toBeTruthy();
    expect(screen.getByText('Wait for the current restart or update to finish.')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Restart' })).toHaveProperty('disabled', true);
  });

  it('says when an update is queued, or the server is up to date', async () => {
    open({ 'GET /api/enshrouded': () => json(status({ update_queued: true })) });
    const { EnshroudedPanel } = await import('@/islands/EnshroudedPanel');
    render(<EnshroudedPanel />);
    expect(await screen.findByText('An update is queued.')).toBeTruthy();
    cleanup();
    open({ 'GET /api/enshrouded': () => json(status({ update_pending: false })) });
    render(<EnshroudedPanel />);
    expect(await screen.findByText(/Up to date: Steam's latest build is from 2 h 30 min ago\./)).toBeTruthy();
  });

  it('asks again every 10 seconds while shown, and at once when shown again', async () => {
    const calls = open({ 'GET /api/enshrouded': () => json(status()) });
    const { EnshroudedPanel, POLL_MS } = await import('@/islands/EnshroudedPanel');
    render(<EnshroudedPanel />);
    await screen.findByText('Chronicle');
    await vi.advanceTimersByTimeAsync(POLL_MS);
    await waitFor(() => expect(calls.length).toBe(2));
    document.dispatchEvent(new Event('visibilitychange'));
    await waitFor(() => expect(calls.length).toBe(3));
  });
});

describe('durations', () => {
  it('read naturally', async () => {
    const { ago, duration } = await import('@/lib/enshrouded');
    expect(duration(45)).toBe('45 s');
    expect(duration(-3)).toBe('0 s');
    expect(duration(12 * 60 + 5)).toBe('12 min');
    expect(duration(3 * 3600 + 5 * 60)).toBe('3 h 5 min');
    expect(duration(2 * 86400 + 4 * 3600 + 59)).toBe('2 d 4 h');
    expect(ago(1000, 990)).toBe('just now');
    expect(ago(10_000, 9_700)).toBe('5 min ago');
  });
});
