// Every translation must translate exactly the keys its source catalog
// defines: the host catalog (`en.ts`) for the bundled host catalogs and for a
// plugin language pack, or a plugin's own `en` catalog for that plugin's
// other locales. A missing key shows up as
// English in the translated UI; an extra key is dead text nobody can see.
import assert from 'node:assert/strict';
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

import { parse } from 'smol-toml';

import { messageParameters } from '../../../../web-sdk/src/i18n/message.ts';
import { CORE_CATALOGS } from './catalogs.ts';
import { en } from './en.ts';

const PLUGINS_DIR = join(
  dirname(fileURLToPath(import.meta.url)),
  '../../../../../plugins',
);

type Catalog = Record<string, string>;

function readCatalog(path: string): Catalog {
  return parse(readFileSync(path, 'utf8')) as Catalog;
}

function compare(source: Catalog, pack: Catalog, label: string) {
  const missing = Object.keys(source).filter((key) => !(key in pack));
  const extra = Object.keys(pack).filter((key) => !(key in source));
  assert.deepEqual(missing, [], `${label} lacks keys`);
  assert.deepEqual(extra, [], `${label} has keys the source does not define`);
  const mismatched = Object.keys(source).filter(
    (key) =>
      messageParameters(source[key]).join() !==
      messageParameters(pack[key]).join(),
  );
  assert.deepEqual(mismatched, [], `${label} changes placeholders`);
}

for (const [locale, catalog] of Object.entries(CORE_CATALOGS)) {
  if (locale === 'en') continue;
  test(`the bundled ${locale} catalog translates exactly the host keys`, () => {
    compare(en, catalog, `lib/i18n ${locale}`);
  });
}

const plugins = readdirSync(PLUGINS_DIR).filter((name) =>
  existsSync(join(PLUGINS_DIR, name, 'plugin.toml')),
);

for (const plugin of plugins) {
  const dir = join(PLUGINS_DIR, plugin);
  const manifest = parse(readFileSync(join(dir, 'plugin.toml'), 'utf8')) as {
    translations?: Record<string, string>;
  };
  const translations = manifest.translations ?? {};
  const source = translations.en ? readCatalog(join(dir, translations.en)) : en;
  for (const [locale, file] of Object.entries(translations)) {
    if (locale === 'en') continue;
    test(`${plugin} ${locale} translates exactly its source keys`, () => {
      compare(source, readCatalog(join(dir, file)), `${plugin}/${file}`);
    });
  }
}
