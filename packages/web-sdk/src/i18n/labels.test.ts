import assert from 'node:assert/strict';
import { test } from 'node:test';

import { identifierLabel, pluginKeys } from './labels.ts';

const catalog: Record<string, string> = {
  'identifier.contestType.icpc': 'ICPC 赛制',
};
const lookup = (key: string) => catalog[key];

test('an identifier a catalog names shows its name', () => {
  assert.equal(identifierLabel(lookup, 'contestType', 'icpc'), 'ICPC 赛制');
});

test('an identifier no catalog names shows as it is', () => {
  assert.equal(identifierLabel(lookup, 'contestType', 'ioi'), 'ioi');
});

test('plugin text keys follow the plugin, schema and field', () => {
  assert.equal(
    pluginKeys.description('cooldown'),
    'plugin.cooldown.description',
  );
  assert.equal(
    pluginKeys.configField(
      'cooldown',
      'cooldown',
      ['limits', 'seconds'],
      'title',
    ),
    'config.cooldown.cooldown.limits.seconds.title',
  );
});
