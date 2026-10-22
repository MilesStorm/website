// Astro discovers client islands in the body, after the synchronous tracing bundle. Put
// their module graphs in the head so downloading can overlap tracing without executing an
// island before fetch instrumentation is ready. Only static imports belong here: lazy
// features must stay lazy. Filenames and dependencies come from this build's client bundle.
import { readdir, readFile, writeFile } from 'node:fs/promises';

/** @param {string} html @param {Map<string, string[]>} imports */
export function preloadIslands(html, imports) {
  const needed = new Set();
  function visit(url) {
    if (needed.has(url)) return;
    const dependencies = imports.get(url);
    if (!dependencies) throw new Error(`Island preload is missing client chunk ${url}`);
    needed.add(url);
    for (const dependency of dependencies) visit(dependency);
  }
  // This is Astro's generated markup, not user HTML. Match whole opening tags so attribute
  // order does not matter, and leave deferred (visible/idle/media) islands alone.
  for (const [tag] of html.matchAll(/<astro-island\b[^>]*>/g)) {
    if (!/\bclient="(?:only|load)"/.test(tag)) continue;
    for (const attribute of ['component-url', 'renderer-url']) {
      const url = tag.match(new RegExp(`\\b${attribute}="([^"]+)"`))?.[1];
      if (url) visit(url);
    }
  }
  if (!needed.size) return html;
  const marker = '<meta name="bff-inject">';
  if (!html.includes(marker)) throw new Error('Island preloads require the BFF head marker');
  const links = [...needed].map((href) => `<link rel="modulepreload" href="${href}" crossorigin>`).join('');
  return html.replace(marker, marker + links);
}

/** @returns {import('astro').AstroIntegration} */
export default function islandPreloads() {
  const imports = new Map();
  return {
    name: 'island-preloads',
    hooks: {
      'astro:build:setup': ({ updateConfig }) => {
        updateConfig({
          plugins: [{
            name: 'island-preload-graph',
            applyToEnvironment: (environment) => environment.name === 'client',
            generateBundle(_options, bundle) {
              imports.clear();
              for (const chunk of Object.values(bundle)) {
                if (chunk.type !== 'chunk') continue;
                imports.set(`/${chunk.fileName}`, chunk.imports.filter((name) => bundle[name]?.type === 'chunk').map((name) => `/${name}`));
              }
            },
          }],
        });
      },
      'astro:build:done': async ({ dir }) => {
        for (const name of await readdir(dir, { recursive: true })) {
          if (!name.endsWith('.html')) continue;
          const file = new URL(name, dir);
          const html = await readFile(file, 'utf8');
          const preloaded = preloadIslands(html, imports);
          if (preloaded !== html) await writeFile(file, preloaded);
        }
      },
    },
  };
}
