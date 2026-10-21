// The browser tracing as one classic script (src/vendor/tracing.js): the Grafana Faro bundles
// (Apache-2.0, with a licence header prepended as they ship without one), OTel's
// BatchSpanProcessor and trace.js (assets/trace.js), in that order. The layout loads it
// synchronously in <head>, before every module script, so fetch and WebSocket are wrapped before
// an island can call them (TRACING.md, "Browser").
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { buildSync } from 'esbuild';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const traceJs = join(root, 'assets/trace.js');
const out = join(root, 'src/vendor/tracing.js');
const parts = [];

for (const pkg of ['faro-web-sdk', 'faro-web-tracing']) {
  const dir = join(root, 'node_modules/@grafana', pkg);
  const { version, license } = JSON.parse(readFileSync(join(dir, 'package.json'), 'utf8'));
  const body = readFileSync(join(dir, 'dist/bundle', `${pkg}.iife.js`), 'utf8')
    .replace(/\n\/\/# sourceMappingURL=.*\s*$/, '\n');
  parts.push(`/*! @grafana/${pkg} ${version} | ${license} | https://github.com/grafana/faro-web-sdk */\n${body}`);
}

// Faro uses OTel's BatchSpanProcessor but its bundle doesn't export it, so it is bundled from
// the same package Faro depends on.
{
  const dir = join(root, 'node_modules/@opentelemetry/sdk-trace-web');
  const { version, license } = JSON.parse(readFileSync(join(dir, 'package.json'), 'utf8'));
  const { outputFiles } = buildSync({
    stdin: { contents: "export { BatchSpanProcessor } from '@opentelemetry/sdk-trace-web';", resolveDir: root },
    bundle: true, format: 'iife', globalName: 'OtelSdkTraceWeb', minify: true, target: 'es2019', write: false,
    banner: { js: `/*! @opentelemetry/sdk-trace-web ${version} BatchSpanProcessor | ${license} | https://github.com/open-telemetry/opentelemetry-js */` },
  });
  parts.push(outputFiles[0].text);
}

parts.push(readFileSync(traceJs, 'utf8'));

mkdirSync(dirname(out), { recursive: true });
// Each part is a complete classic script; `;` keeps one from continuing the last statement of
// the one before it.
writeFileSync(out, parts.join('\n;\n'));
console.log(`vendored the tracing script -> src/vendor/tracing.js`);
