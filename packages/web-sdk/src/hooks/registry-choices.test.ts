import assert from 'node:assert/strict';
import { test } from 'node:test';

import { registryChoices } from './registry-choices.ts';

test('a stored value the registries list is one of them', () => {
  assert.deepEqual(registryChoices(['batch', 'communication'], 'batch'), [
    { value: 'batch', unavailable: false },
    { value: 'communication', unavailable: false },
  ]);
});

test('a stored value the loaded registries do not list stays, unavailable', () => {
  assert.deepEqual(registryChoices(['communication'], 'batch'), [
    { value: 'communication', unavailable: false },
    { value: 'batch', unavailable: true },
  ]);
  assert.deepEqual(registryChoices([], 'batch'), [
    { value: 'batch', unavailable: true },
  ]);
});

test('registries not loaded show the stored value on its own', () => {
  assert.deepEqual(registryChoices(undefined, 'batch'), [
    { value: 'batch', unavailable: false },
  ]);
  assert.deepEqual(registryChoices(undefined, ''), []);
});

test('no stored value offers what is registered', () => {
  assert.deepEqual(registryChoices(['ioi'], ''), [
    { value: 'ioi', unavailable: false },
  ]);
});
