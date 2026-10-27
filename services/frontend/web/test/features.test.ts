import { describe, expect, it } from 'vitest';
import { homeCards, previewCards } from '@/lib/features';

describe('the start page cards', () => {
  it('open the ones the user has, then lock the advertised rest', () => {
    const cards = homeCards(['llama']).map((c) => [c.feature.to, c.state]);
    expect(cards).toEqual([
      ['/ark', 'open'],
      ['/arcane', 'locked'],
    ]);
  });

  it('show everything to someone with every permission', () => {
    const cards = homeCards(['arcane', 'llama', 'enshrouded', 'manage_permissions']);
    expect(cards.every((c) => c.state === 'open')).toBe(true);
    expect(cards).toHaveLength(3);
  });

  it('preview the advertised features when logged out', () => {
    expect(previewCards().map((f) => f.to)).toEqual(['/arcane', '/login']);
  });
});
