// Embeds the language packs shipped in this repository into the web build,
// as the catalogs the UI has before (and without) the server. A language
// pack is a plugin whose manifest declares only translations; the server
// serves the same packs, and whatever it serves overrides these.
//
// The output, `src/lib/i18n/packs.gen.ts`, is build output: it is generated
// by the Vite plugin below on every dev start and build, and by
// `pnpm gen:language-packs` before type checks and lint.

import { readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { parse } from 'smol-toml';

const WEB_ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
export const PLUGINS_DIR = join(WEB_ROOT, '../../plugins');
const OUTPUT = join(WEB_ROOT, 'src/lib/i18n/packs.gen.ts');

/** The locale every other one falls back to, and the source of the key type. */
export const SOURCE_LOCALE = 'en';

/** Manifest tables that do not stop a plugin being a language pack. */
const PACK_TABLES = new Set(['translations', 'bundle']);

type Catalog = Record<string, string>;

/** The language packs under `plugins/`: each locale and its catalog file. */
export function languagePackFiles(): Map<string, string> {
  const files = new Map<string, string>();
  for (const name of readdirSync(PLUGINS_DIR).sort()) {
    const dir = join(PLUGINS_DIR, name);
    let manifest: Record<string, unknown>;
    try {
      manifest = parse(readFileSync(join(dir, 'plugin.toml'), 'utf8'));
    } catch {
      continue;
    }
    const tables = Object.entries(manifest).filter(
      ([, value]) => typeof value === 'object' && value !== null,
    );
    if (!tables.every(([table]) => PACK_TABLES.has(table))) continue;
    const translations = (manifest.translations ?? {}) as Catalog;
    for (const [locale, file] of Object.entries(translations)) {
      if (files.has(locale)) {
        throw new Error(`two language packs translate ${locale}: ${name}`);
      }
      files.set(locale, join(dir, file));
    }
  }
  if (!files.has(SOURCE_LOCALE)) {
    throw new Error(`no language pack translates ${SOURCE_LOCALE}`);
  }
  return files;
}

export function readCatalog(file: string): Catalog {
  return parse(readFileSync(file, 'utf8')) as Catalog;
}

/** Write `packs.gen.ts` from the language packs. */
export function generateLanguagePacks(): void {
  const packs = [...languagePackFiles()].map(
    ([locale, file]) => [locale, readCatalog(file)] as const,
  );
  const source = packs.find(([locale]) => locale === SOURCE_LOCALE)![1];
  const literal = (catalog: Catalog) =>
    `{\n${Object.entries(catalog)
      .map(
        ([key, value]) => `  ${JSON.stringify(key)}: ${JSON.stringify(value)},`,
      )
      .join('\n')}\n}`;
  const text = `// Generated from the language packs in plugins/ by scripts/language-packs.ts.
// Do not edit: change the pack and rebuild.

/** The source catalog: its keys are the host's message keys. */
export const sourceCatalog = ${literal(source)};

/** Every embedded catalog, by locale. */
export const embeddedCatalogs: Record<string, Record<string, string>> = {
${packs
  .map(([locale, catalog]) =>
    locale === SOURCE_LOCALE
      ? `  ${JSON.stringify(locale)}: sourceCatalog,`
      : `  ${JSON.stringify(locale)}: ${literal(catalog).replaceAll('\n', '\n  ')},`,
  )
  .join('\n')}
};
`;
  let current = '';
  try {
    current = readFileSync(OUTPUT, 'utf8');
  } catch {
    // Not generated yet.
  }
  if (current !== text) writeFileSync(OUTPUT, text);
}

/**
 * Regenerates the packs when dev or a build starts, and in dev when a pack
 * file changes (a new pack is picked up on the next start).
 */
export function languagePacksPlugin() {
  return {
    name: 'broccoli-language-packs',
    buildStart() {
      generateLanguagePacks();
    },
    configureServer(server: {
      watcher: {
        add: (path: string) => void;
        on: (event: string, listener: (path: string) => void) => void;
      };
    }) {
      const files = new Set(languagePackFiles().values());
      for (const file of files) server.watcher.add(file);
      server.watcher.on('change', (path) => {
        if (files.has(path)) generateLanguagePacks();
      });
    },
  };
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  generateLanguagePacks();
}
