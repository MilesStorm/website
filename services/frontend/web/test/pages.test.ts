import { describe, expect, it } from 'vitest';
import { PAGE_PATHS, isPagePath } from '@/lib/pages';
import spec from '@/api/openapi.json';

describe('the pages', () => {
  it('are the server’s PAGES table', () => {
    const served = (spec as unknown as { 'x-pages': { path: string }[] })['x-pages'].map((p) => p.path);
    expect([...PAGE_PATHS].sort()).toEqual(served.sort());
  });

  it('are only exact paths', () => {
    expect(isPagePath('/')).toBe(true);
    for (const bad of ['//evil.com', '/asshole/', 'https://milesstorm.com/', '/api/session', '/arcane']) {
      expect(isPagePath(bad)).toBe(false);
    }
  });
});
