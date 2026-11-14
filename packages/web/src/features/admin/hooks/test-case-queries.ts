import type { QueryClient } from '@tanstack/react-query';

/** The cache key of a problem's test case list. */
export function testCasesQueryKey(problemId: number) {
  return ['test-cases', problemId];
}

/** The cache key of the problem's stored configs its plugins now refuse. */
export function problemConfigIssuesQueryKey(problemId: number) {
  return ['problem-config-issues', problemId];
}

/**
 * Refetch everything a change to a problem's test cases affects: the list,
 * the problem (its test case count), its sample contents and the configs
 * that refer to its test cases.
 */
export function invalidateProblemTestCases(
  queryClient: QueryClient,
  problemId: number,
) {
  queryClient.invalidateQueries({ queryKey: testCasesQueryKey(problemId) });
  queryClient.invalidateQueries({ queryKey: ['problem', problemId] });
  queryClient.invalidateQueries({
    queryKey: ['problem-sample-contents', problemId],
  });
  queryClient.invalidateQueries({
    queryKey: problemConfigIssuesQueryKey(problemId),
  });
}
