import { readdir, readFile, writeFile } from 'node:fs/promises';
import { extname, join, resolve } from 'node:path';
import { promisify } from 'node:util';
import { brotliCompress, constants, gzip } from 'node:zlib';

import type { Plugin, ResolvedConfig } from 'vite';

const brotli = promisify(brotliCompress);
const gz = promisify(gzip);

/** Text formats worth compressing; images and woff fonts already are. */
const COMPRESSIBLE = new Set(['.js', '.css', '.json', '.svg', '.ttf', '.txt']);

/** Below this the headers outweigh the saving. */
const MIN_BYTES = 1024;

/**
 * The `.br` and `.gz` siblings of every compressible file in `dir`, as
 * `[path, bytes]`. A variant that is not smaller than the file is left out.
 */
export async function compressedVariants(
  dir: string,
): Promise<[string, Uint8Array][]> {
  const names = await readdir(dir);
  const variants = await Promise.all(
    names
      .filter((name) => COMPRESSIBLE.has(extname(name)))
      .map(async (name) => {
        const path = join(dir, name);
        const bytes = await readFile(path);
        if (bytes.length < MIN_BYTES) return [];
        // The async zlib calls run on libuv's thread pool, so the files
        // compress in parallel.
        const [br, gzipped] = await Promise.all([
          brotli(bytes, {
            params: {
              [constants.BROTLI_PARAM_QUALITY]: constants.BROTLI_MAX_QUALITY,
              [constants.BROTLI_PARAM_SIZE_HINT]: bytes.length,
            },
          }),
          gz(bytes, { level: constants.Z_BEST_COMPRESSION }),
        ]);
        return [
          [`${path}.br`, br],
          [`${path}.gz`, gzipped],
        ].filter(([, out]) => out.length < bytes.length) as [
          string,
          Uint8Array,
        ][];
      }),
  );
  return variants.flat();
}

/**
 * Writes brotli and gzip copies of the client build's content-hashed assets,
 * which the server sends instead of the file to a browser that accepts them:
 * smaller than compressing per request, and no CPU spent on it during a
 * contest. Only `assets/` is covered: `index.html` is written after this
 * build and is small, so the server compresses it per request.
 */
export function precompressPlugin(): Plugin {
  let config: ResolvedConfig;
  return {
    name: 'broccoli:precompress',
    apply: 'build',
    configResolved(resolved) {
      config = resolved;
    },
    async closeBundle() {
      if (config.build.ssr) return;
      const assets = resolve(config.root, config.build.outDir, 'assets');
      for (const [path, bytes] of await compressedVariants(assets)) {
        await writeFile(path, bytes);
      }
    },
  };
}
