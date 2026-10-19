import { beforeEach, describe, expect, it, vi } from 'vitest';
import { ann, json, loggedOut, splice } from './fixtures';

beforeEach(() => {
  vi.resetModules();
  document.head.innerHTML = '';
  delete document.documentElement.dataset.session;
  delete document.documentElement.dataset.perms;
});

describe('the viewer', () => {
  it('comes from the spliced tag without asking the server', async () => {
    const fetch = vi.fn();
    vi.stubGlobal('fetch', fetch);
    splice(ann);
    const { readBootstrap } = await import('@/api/session');
    expect(await readBootstrap()).toEqual(ann);
    expect(fetch).not.toHaveBeenCalled();
  });

  it('is asked for once when the page has no tag', async () => {
    const fetch = vi.fn(async () => json(ann));
    vi.stubGlobal('fetch', fetch);
    const { readBootstrap } = await import('@/api/session');
    expect(await readBootstrap()).toEqual(ann);
    expect(await readBootstrap()).toEqual(ann);
    expect(fetch).toHaveBeenCalledOnce();
  });

  it('is logged out when the server fails', async () => {
    vi.stubGlobal('fetch', async () => json({ error: 'internal', message: 'x' }, 500));
    const { fetchBootstrap } = await import('@/api/session');
    expect(await fetchBootstrap()).toEqual(loggedOut);
  });

  it('is written on <html> as the server writes it', async () => {
    const { applySession } = await import('@/api/session');
    applySession(ann);
    expect(document.documentElement.dataset).toMatchObject({ session: 'in', perms: 'arcane llama' });
    applySession(loggedOut);
    expect(document.documentElement.dataset).toMatchObject({ session: 'out', perms: '' });
  });

  it('changes when the user or their permissions do', async () => {
    const { sameViewer } = await import('@/api/session');
    expect(sameViewer(ann, structuredClone(ann))).toBe(true);
    expect(sameViewer(ann, loggedOut)).toBe(false);
    expect(sameViewer(ann, { viewer: { ...ann.viewer!, permissions: ['arcane'] } })).toBe(false);
  });
});
