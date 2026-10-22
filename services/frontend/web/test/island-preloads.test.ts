import { describe, expect, it } from 'vitest';
import { preloadIslands } from '../scripts/island-preloads.mjs';

const graph = new Map([
  ['/_astro/Home.hash.js', ['/_astro/shared.hash.js']],
  ['/_astro/Menu.hash.js', ['/_astro/shared.hash.js']],
  ['/_astro/react.hash.js', ['/_astro/shared.hash.js']],
  ['/_astro/shared.hash.js', ['/_astro/leaf.hash.js']],
  // Cycles must not duplicate links or recurse forever.
  ['/_astro/leaf.hash.js', ['/_astro/shared.hash.js']],
  ['/_astro/OtherPage.hash.js', []],
]);

describe('island module discovery', () => {
  it('preloads just this page’s eager islands, renderer and transitive static imports before tracing', () => {
    const html = '<head><meta charset="utf-8"><meta name="bff-inject"><script src="/tracing.js"></script></head><body>'
      + '<astro-island client="only" component-url="/_astro/Home.hash.js" renderer-url="/_astro/react.hash.js"></astro-island>'
      + '<astro-island component-url="/_astro/Menu.hash.js" renderer-url="/_astro/react.hash.js" client="load"></astro-island>'
      + '<astro-island client="visible" component-url="/_astro/OtherPage.hash.js"></astro-island></body>';
    const result = preloadIslands(html, graph);
    const document = new DOMParser().parseFromString(result, 'text/html');
    const links = [...document.querySelectorAll<HTMLLinkElement>('link[rel="modulepreload"]')];
    expect(links.map((link) => link.getAttribute('href'))).toEqual([
      '/_astro/Home.hash.js', '/_astro/shared.hash.js', '/_astro/leaf.hash.js', '/_astro/react.hash.js', '/_astro/Menu.hash.js',
    ]);
    expect(links.every((link) => link.hasAttribute('crossorigin'))).toBe(true);
    expect(result.indexOf('modulepreload')).toBeLessThan(result.indexOf('<script'));
    expect(result.endsWith(html.slice(html.indexOf('<script')))).toBe(true);
    // Preloading must never introduce execution or change the tracing script's attributes.
    expect(document.querySelectorAll('script')).toHaveLength(1);
    expect(document.querySelector('script')?.outerHTML).toBe('<script src="/tracing.js"></script>');
  });

  it('leaves pages without eager islands alone', () => {
    const html = '<head></head><body><astro-island client="idle" component-url="/lazy.js"></astro-island></body>';
    expect(preloadIslands(html, graph)).toBe(html);
  });

  it('fails the build if an eager island cannot be resolved from client output', () => {
    expect(() => preloadIslands('<meta name="bff-inject"><astro-island client="only" component-url="/missing.js">', graph))
      .toThrow('missing client chunk /missing.js');
  });
});
