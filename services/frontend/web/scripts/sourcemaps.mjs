// Moves the build's source maps out of what is served (dist/_astro/*.map) into sourcemaps/,
// which the image keeps beside the pages but never serves (SOURCE_MAPS). The server maps browser
// error stacks with them before they go to Alloy (crates/bff/src/telemetry.rs). The scripts carry
// no sourceMappingURL comment (`sourcemap: 'hidden'` in astro.config.mjs, `external` in
// scripts/tracing.mjs), so nothing asks for them.
import { mkdirSync, readdirSync, renameSync, rmSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const from = join(root, 'dist/_astro');
const to = join(root, 'sourcemaps');

rmSync(to, { recursive: true, force: true });
mkdirSync(to, { recursive: true });
let moved = 0;
for (const name of readdirSync(from)) {
  if (!name.endsWith('.js.map')) continue;
  renameSync(join(from, name), join(to, name));
  moved++;
}
console.log(`${moved} source maps -> sourcemaps/`);
