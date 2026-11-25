import assert from 'node:assert/strict';
import { test } from 'node:test';

import { submissionViewKeys } from '@broccoli/web-sdk/submission';
import { QueryClient } from '@tanstack/react-query';

import { rereadSubmissionViews } from './use-follow-submission-visibility.ts';

test('a visibility change reads every view of a submission again, and nothing else', () => {
  const queryClient = new QueryClient();
  const views = [
    submissionViewKeys.detail(9),
    submissionViewKeys.judgements(9),
    submissionViewKeys.judgement(9, 3),
    submissionViewKeys.testCases(9, 3, 1, '101,102', undefined, false, false),
    submissionViewKeys.list('contest', '4', 'all'),
    // A plugin's own view of what the viewer may see of a submission.
    submissionViewKeys.plugin('some-plugin', 'token-status', 4),
  ];
  const other = ['some-plugin-settings', 4];
  for (const key of [...views, other]) queryClient.setQueryData(key, {});

  rereadSubmissionViews(queryClient);

  for (const key of views) {
    assert.equal(
      queryClient.getQueryState(key)?.isInvalidated,
      true,
      String(key),
    );
  }
  assert.equal(queryClient.getQueryState(other)?.isInvalidated, false);
});
