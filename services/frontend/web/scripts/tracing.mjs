// The browser telemetry as one classic script: assets/main.js (Grafana Faro's modules, OTel's
// BatchSpanProcessor and assets/trace.js) bundled, tree-shaken and minified into
// public/_astro/tracing.<hash>.js, with its source map next to it. src/vendor/tracing.json names
// the file for the layout, which loads it synchronously in <head>, before every module script,
// so fetch and WebSocket are wrapped before an island can call them (TRACING.md, "Browser").
// Astro copies public/ into dist/, so the script is served and cached like Astro's own files.
import { mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const outdir = join(root, 'public/_astro');
const manifest = join(root, 'src/vendor/tracing.json');

mkdirSync(outdir, { recursive: true });
for (const name of readdirSync(outdir)) {
  if (name.startsWith('tracing.')) rmSync(join(outdir, name));
}

// Faro's exporter imports @opentelemetry/otlp-transformer by its CommonJS paths (build/src/…),
// which can't be tree-shaken and bring a CommonJS copy of @opentelemetry/core and every semantic
// convention with them (75 KB of the minified script). The package ships the same modules as ES
// modules under build/esm/.
const otlpAsEsm = {
  name: 'otlp-transformer-esm',
  setup(b) {
    b.onResolve({ filter: /^@opentelemetry\/otlp-transformer\/build\/src\// }, (args) =>
      b.resolve(args.path.replace('/build/src/', '/build/esm/'), { kind: args.kind, resolveDir: args.resolveDir }));
  },
};

// The libraries ship without licence headers; the script names them.
const version = (pkg) => JSON.parse(readFileSync(join(root, 'node_modules', pkg, 'package.json'), 'utf8')).version;
const banner = `/*! Grafana Faro ${version('@grafana/faro-web-sdk')} (https://github.com/grafana/faro-web-sdk) and ` +
  `OpenTelemetry JS ${version('@opentelemetry/sdk-trace-web')} (https://github.com/open-telemetry/opentelemetry-js), Apache-2.0 */`;

const { metafile } = await build({
  entryPoints: [join(root, 'assets/main.js')],
  outdir,
  entryNames: 'tracing.[hash]',
  bundle: true,
  format: 'iife',
  minify: true,
  sourcemap: 'linked',
  // The sources in the map are the repository's (public), not node_modules' only.
  sourcesContent: true,
  target: 'es2020',
  banner: { js: banner },
  metafile: true,
  plugins: [otlpAsEsm],
  logLevel: 'warning',
});

const [js] = Object.keys(metafile.outputs).filter((f) => f.endsWith('.js'));
const src = '/' + relative(join(root, 'public'), join(root, js)).split('\\').join('/');
mkdirSync(dirname(manifest), { recursive: true });
writeFileSync(manifest, JSON.stringify({ src }) + '\n');
console.log(`tracing script -> public${src} (${metafile.outputs[js].bytes} bytes)`);
