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

test('a visibility change announced in one tab reaches the listeners of another', async () => {
  // A second instance of the module is the same app in another tab.
  const other = (await import(
    './visibility.ts?another-tab'
  )) as typeof import('./visibility.ts');
  const heard = new Promise<number>((resolve) => {
    const stop = other.onSubmissionVisibilityChange((id) => {
      stop();
      resolve(id);
    });
  });
  announceSubmissionVisibilityChange(11);
  assert.equal(await heard, 11);
});
