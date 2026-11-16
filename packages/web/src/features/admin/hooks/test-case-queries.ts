import type { QueryClient } from '@tanstack/react-query';

/** The cache key of a problem's test case list. */
export function testCasesQueryKey(problemId: number) {
  return ['test-cases', problemId];
}

/**
 * Refetch everything a change to a problem's test cases affects: the list,
 * the problem (its test case count) and its sample contents.
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
}
