import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  announceSubmissionVisibilityChange,
  onSubmissionVisibilityChange,
} from './visibility.ts';

test('a visibility change reaches every listener until it unsubscribes', () => {
  const heard: string[] = [];
  const stopA = onSubmissionVisibilityChange((id) => heard.push(`a${id}`));
  const stopB = onSubmissionVisibilityChange((id) => heard.push(`b${id}`));
  announceSubmissionVisibilityChange(7);
  stopA();
  announceSubmissionVisibilityChange(8);
  stopB();
  announceSubmissionVisibilityChange(9);
  assert.deepEqual(heard, ['a7', 'b7', 'b8']);
});
