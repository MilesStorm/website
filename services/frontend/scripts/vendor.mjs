// Copies the Grafana Faro browser bundles (Apache-2.0) into packages/web/assets/vendor/,
// where the web package's `asset!` macros pick them up. The bundles ship without a
// licence header, so one is prepended.
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
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
