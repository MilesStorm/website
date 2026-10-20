// Brotli and gzip copies of Astro's files (dist/_astro), next to them: the server sends the
// copy the browser accepts (ServeDir's precompressed files), with no compression at run time.
// The pages aren't compressed: the server splices each request's data into them.
import { readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { dirname, extname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { brotliCompressSync, constants, gzipSync } from 'node:zlib';

const dir = join(dirname(fileURLToPath(import.meta.url)), '../dist/_astro');
// Fonts and images are compressed already.
const TEXT = new Set(['.js', '.mjs', '.css', '.svg', '.json', '.map']);

for (const name of readdirSync(dir, { recursive: true })) {
  const file = join(dir, name);
  if (!statSync(file).isFile() || !TEXT.has(extname(file))) continue;
  const body = readFileSync(file);
  writeFileSync(`${file}.br`, brotliCompressSync(body, { params: { [constants.BROTLI_PARAM_QUALITY]: 11 } }));
  writeFileSync(`${file}.gz`, gzipSync(body, { level: 9 }));
}
