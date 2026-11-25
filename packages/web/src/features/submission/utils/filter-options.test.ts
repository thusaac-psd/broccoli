import assert from 'node:assert/strict';
import { test } from 'node:test';

import { filterOptions } from './filter-options.ts';

const labels = { all: 'All languages', allUnavailable: 'All languages (?)' };
const cpp = { value: 'cpp', label: 'C++' };

test('every listed choice follows "all"', () => {
  assert.deepEqual(filterOptions(labels, [cpp], false, null), [
    { value: 'all', label: 'All languages' },
    { value: 'cpp', label: 'C++' },
  ]);
});

test('a list that failed says so, and the applied filter stays a choice', () => {
  assert.deepEqual(filterOptions(labels, undefined, true, 'py'), [
    { value: 'all', label: 'All languages (?)' },
    { value: 'py', label: 'py' },
  ]);
});

test('an applied filter the list names is not listed twice', () => {
  assert.equal(filterOptions(labels, [cpp], false, 'cpp').length, 2);
});

test('a problem filter applied before the problems load names its id', () => {
  assert.deepEqual(filterOptions(labels, undefined, false, '3'), [
    { value: 'all', label: 'All languages' },
    { value: '3', label: '3' },
  ]);
});
