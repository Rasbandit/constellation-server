// Writes a .gz and a .br next to every text file in dist/, so the server sends the stored bytes
// instead of compressing per request. Run after `vite build`.
import { readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { brotliCompressSync, constants, gzipSync } from 'node:zlib';

const TEXT = /\.(html|js|mjs|css|json|svg|txt|webmanifest)$/;
const MIN = 256; // smaller files do not pay for the header

function* files(dir) {
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) yield* files(path);
    else if (TEXT.test(name) && statSync(path).size >= MIN) yield path;
  }
}

let n = 0;
for (const path of files(new URL('../dist', import.meta.url).pathname)) {
  const body = readFileSync(path);
  writeFileSync(`${path}.gz`, gzipSync(body, { level: 9 }));
  writeFileSync(`${path}.br`, brotliCompressSync(body, { params: { [constants.BROTLI_PARAM_QUALITY]: 11 } }));
  n += 1;
}
console.log(`precompressed ${n} files`);
