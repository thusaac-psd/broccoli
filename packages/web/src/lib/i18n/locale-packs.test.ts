// Translation completeness. Every catalog a plugin ships translates exactly
// the keys of its source: the English language pack for the other language
// packs (they translate the host's messages), or a plugin's own `en` catalog
// for that plugin's other locales. A failure lists the missing keys (shown
// in English in the translated UI), the extra ones (dead text nobody sees),
// and those whose placeholders differ.
import assert from 'node:assert/strict';
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';

import { parse } from 'smol-toml';

import { messageParameters } from '../../../../web-sdk/src/i18n/message.ts';
import {
  languagePackFiles,
  PLUGINS_DIR,
  readCatalog,
  SOURCE_LOCALE,
} from '../../../scripts/language-packs.ts';

type Catalog = Record<string, string>;

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

const hostMessages = readCatalog(languagePackFiles().get(SOURCE_LOCALE)!);

const plugins = readdirSync(PLUGINS_DIR).filter((name) =>
  existsSync(join(PLUGINS_DIR, name, 'plugin.toml')),
);

for (const plugin of plugins) {
  const dir = join(PLUGINS_DIR, plugin);
  const manifest = parse(readFileSync(join(dir, 'plugin.toml'), 'utf8')) as {
    translations?: Record<string, string>;
  };
  const translations = manifest.translations ?? {};
  const source = translations[SOURCE_LOCALE]
    ? readCatalog(join(dir, translations[SOURCE_LOCALE]))
    : hostMessages;
  for (const [locale, file] of Object.entries(translations)) {
    if (locale === SOURCE_LOCALE) continue;
    test(`${plugin} ${locale} translates exactly its source keys`, () => {
      compare(source, readCatalog(join(dir, file)), `${plugin}/${file}`);
    });
  }
}
