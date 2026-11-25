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

import { pluginKeys } from '../../../../web-sdk/src/i18n/labels.ts';
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

interface SchemaProperty {
  title?: string;
  description?: string;
  unit?: string;
  enum?: unknown[];
  enum_titles?: string[];
  properties?: Record<string, SchemaProperty>;
  items?: SchemaProperty;
}

interface Namespace {
  title?: string;
  description?: string;
  properties?: Record<string, SchemaProperty>;
}

/**
 * The English of a plugin's config schemas, under the keys the settings
 * form looks their translations up by (`pluginKeys`): the schema is the
 * source of that text, so the English pack need not repeat it, and every
 * other pack translates it.
 */
function schemaText(
  plugin: string,
  config: Record<string, Namespace>,
): Catalog {
  const text: Catalog = {};
  for (const [namespace, ns] of Object.entries(config)) {
    if (ns.title) {
      text[pluginKeys.configTitle(plugin, namespace)] = ns.title;
    }
    if (ns.description) {
      text[pluginKeys.configDescription(plugin, namespace)] = ns.description;
    }
    const walk = (
      path: string[],
      properties: Record<string, SchemaProperty>,
    ) => {
      for (const [name, property] of Object.entries(properties)) {
        const at = [...path, name];
        for (const part of ['title', 'description', 'unit'] as const) {
          const value = property[part];
          if (value) {
            text[pluginKeys.configField(plugin, namespace, at, part)] = value;
          }
        }
        property.enum_titles?.forEach((title, index) => {
          const value = String(property.enum?.[index]);
          text[pluginKeys.configOption(plugin, namespace, at, value)] = title;
        });
        if (property.properties) walk(at, property.properties);
        if (property.items?.properties) walk(at, property.items.properties);
      }
    };
    walk([], ns.properties ?? {});
  }
  return text;
}

for (const plugin of plugins) {
  const dir = join(PLUGINS_DIR, plugin);
  const manifest = parse(readFileSync(join(dir, 'plugin.toml'), 'utf8')) as {
    translations?: Record<string, string>;
    config?: Record<string, Namespace>;
  };
  const translations = manifest.translations ?? {};
  const source = translations[SOURCE_LOCALE]
    ? {
        ...readCatalog(join(dir, translations[SOURCE_LOCALE])),
        ...schemaText(plugin, manifest.config ?? {}),
      }
    : hostMessages;
  for (const [locale, file] of Object.entries(translations)) {
    if (locale === SOURCE_LOCALE) continue;
    test(`${plugin} ${locale} translates exactly its source keys`, () => {
      compare(source, readCatalog(join(dir, file)), `${plugin}/${file}`);
    });
  }
}

// A plugin with several namespaces shows each as a tab of its settings, which
// its title labels: without one the tab reads the namespace's raw id.
test('every namespace of a plugin with several has a title', () => {
  const untitled = plugins.flatMap((plugin) => {
    const manifest = parse(
      readFileSync(join(PLUGINS_DIR, plugin, 'plugin.toml'), 'utf8'),
    ) as { config?: Record<string, Namespace> };
    const config = manifest.config ?? {};
    const namespaces = Object.keys(config);
    return namespaces.length > 1
      ? namespaces
          .filter((namespace) => !config[namespace].title)
          .map((namespace) => `${plugin}/${namespace}`)
      : [];
  });
  assert.deepEqual(untitled, [], 'namespaces without a title');
});

// An enum's options read as their titles: without them the settings form
// shows each option's raw value.
test('every enum option a plugin declares has a title', () => {
  const untitled: string[] = [];
  const walk = (
    at: string,
    properties: Record<string, SchemaProperty & { items?: SchemaProperty }>,
  ) => {
    for (const [name, property] of Object.entries(properties)) {
      const path = `${at}.${name}`;
      if (
        property.enum &&
        property.enum_titles?.length !== property.enum.length
      ) {
        untitled.push(path);
      }
      if (property.properties) walk(path, property.properties);
      if (property.items?.properties) walk(path, property.items.properties);
    }
  };
  for (const plugin of plugins) {
    const manifest = parse(
      readFileSync(join(PLUGINS_DIR, plugin, 'plugin.toml'), 'utf8'),
    ) as { config?: Record<string, Namespace> };
    for (const [namespace, ns] of Object.entries(manifest.config ?? {})) {
      walk(`${plugin}/${namespace}`, ns.properties ?? {});
    }
  }
  assert.deepEqual(untitled, [], 'enums without a title for each option');
});
