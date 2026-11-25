import assert from 'node:assert/strict';
import { test } from 'node:test';

import { languageFilterOptions } from './language-filter.ts';

const labels = { all: 'All languages', allUnavailable: 'All languages (?)' };
const cpp = { id: 'cpp', name: 'C++' };

test('every supported language follows "all"', () => {
  assert.deepEqual(languageFilterOptions(labels, [cpp], false, null), [
    { value: 'all', label: 'All languages' },
    { value: 'cpp', label: 'C++' },
  ]);
});

test('a list that failed says so, and the applied filter stays a choice', () => {
  assert.deepEqual(languageFilterOptions(labels, undefined, true, 'py'), [
    { value: 'all', label: 'All languages (?)' },
    { value: 'py', label: 'py' },
  ]);
});

test('an applied filter the list names is not listed twice', () => {
  assert.equal(languageFilterOptions(labels, [cpp], false, 'cpp').length, 2);
});
