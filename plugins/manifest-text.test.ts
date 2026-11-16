// What a plugin's manifest shows in the UI (its name and description, and
// the contest types it judges) reads in the user's language: each plugin
// names itself in its own translation packs (see `i18n/labels.ts` in the web
// SDK), in every UI language the repository ships.
import assert from 'node:assert/strict';
import { readdirSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

import { parse } from 'smol-toml';

const PLUGINS = dirname(fileURLToPath(import.meta.url));
const UI_LOCALES = ['en', 'zh-CN'];

interface Manifest {
  description?: string;
  language_pack?: { locale: string };
  translations?: Record<string, string>;
}

const read = (path: string) => readFileSync(join(PLUGINS, path), 'utf8');

const plugins = readdirSync(PLUGINS, { withFileTypes: true })
  .filter((entry) => entry.isDirectory())
  .map((entry) => entry.name)
  .filter((id) => {
    try {
      read(`${id}/plugin.toml`);
      return true;
    } catch {
      return false;
    }
  });

test('every plugin names itself and its contest types in every UI language', () => {
  const missing: string[] = [];
  for (const id of plugins) {
    const source = read(`${id}/plugin.toml`);
    const manifest = parse(source) as Manifest;
    const translations = manifest.translations ?? {};
    // A language pack names itself in its own language only.
    const locales = manifest.language_pack
      ? [manifest.language_pack.locale]
      : UI_LOCALES;
    const contestTypes = new Set(
      [...source.matchAll(/^contest_type\s*=\s*"([^"]+)"/gm)].map((m) => m[1]),
    );
    const keys = [
      `plugin.${id}.name`,
      ...(manifest.description ? [`plugin.${id}.description`] : []),
      ...[...contestTypes].map((type) => `identifier.contestType.${type}`),
    ];
    for (const locale of locales) {
      const file = translations[locale];
      const pack = file
        ? (parse(read(`${id}/${file}`)) as Record<string, unknown>)
        : {};
      for (const key of keys) {
        if (typeof pack[key] !== 'string')
          missing.push(`${id} ${locale}: ${key}`);
      }
    }
  }
  assert.deepEqual(missing, []);
});
