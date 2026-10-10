// Copies the Grafana Faro browser bundles (Apache-2.0) into packages/web/assets/vendor/,
// where the web package's `asset!` macros pick them up. The bundles ship without a
// licence header, so one is prepended.
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { buildSync } from 'esbuild';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const out = join(root, 'packages/web/assets/vendor');
mkdirSync(out, { recursive: true });

for (const pkg of ['faro-web-sdk', 'faro-web-tracing']) {
  const dir = join(root, 'node_modules/@grafana', pkg);
  const { version, license } = JSON.parse(readFileSync(join(dir, 'package.json'), 'utf8'));
  const file = `${pkg}.iife.js`;
  const body = readFileSync(join(dir, 'dist/bundle', file), 'utf8')
    .replace(/\n\/\/# sourceMappingURL=.*\s*$/, '\n');
  writeFileSync(join(out, file), `/*! @grafana/${pkg} ${version} | ${license} | https://github.com/grafana/faro-web-sdk */\n${body}`);
  console.log(`vendored @grafana/${pkg}@${version} -> packages/web/assets/vendor/${file}`);
}

// OTel's BatchSpanProcessor, which trace.js puts behind its own span processor. Faro uses it
// too but its bundle doesn't export it, so it is bundled from the same package Faro depends on.
{
  const dir = join(root, 'node_modules/@opentelemetry/sdk-trace-web');
  const { version, license } = JSON.parse(readFileSync(join(dir, 'package.json'), 'utf8'));
  const file = 'otel-batch.iife.js';
  buildSync({
    stdin: { contents: "export { BatchSpanProcessor } from '@opentelemetry/sdk-trace-web';", resolveDir: root },
    bundle: true, format: 'iife', globalName: 'OtelSdkTraceWeb', minify: true, target: 'es2019',
    banner: { js: `/*! @opentelemetry/sdk-trace-web ${version} BatchSpanProcessor | ${license} | https://github.com/open-telemetry/opentelemetry-js */` },
    outfile: join(out, file),
  });
  console.log(`vendored @opentelemetry/sdk-trace-web@${version} BatchSpanProcessor -> packages/web/assets/vendor/${file}`);
}
