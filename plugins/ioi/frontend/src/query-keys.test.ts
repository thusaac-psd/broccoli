import assert from 'node:assert/strict';
import { test } from 'node:test';

import { submissionViewKeys } from '@broccoli/web-sdk/submission';
import { QueryClient } from '@tanstack/react-query';

import { ioiKeys } from './query-keys.ts';

test('every IOI query that reads what the viewer may see is read again when it changes', () => {
  const queryClient = new QueryClient();
  const visibilityDependent = [
    ioiKeys.tokenStatus(4),
    ioiKeys.subtaskScores(4, 9, 1, 'Accepted'),
    ioiKeys.scoreboard(4),
  ];
  for (const key of visibilityDependent) queryClient.setQueryData(key, {});

  // What the host does when a submission's visibility changes.
  void queryClient.invalidateQueries({ queryKey: submissionViewKeys.all });

  for (const key of visibilityDependent) {
    assert.equal(
      queryClient.getQueryState(key)?.isInvalidated,
      true,
      String(key),
    );
  }
});
