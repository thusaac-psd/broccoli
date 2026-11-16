import type { TestCaseResult } from '@broccoli/web-sdk/submission';

export type DisplayTestCaseResult = TestCaseResult & {
  isPlaceholder?: boolean;
  /** A subtask member that names no test case, shown to staff to fix. */
  isMissing?: boolean;
  label?: string;
};

/**
 * The cell of subtask member `label`. The task config's `labelMap` is the one
 * resolution of a member to its test case: a member it resolves shows that
 * case's result, or a pending placeholder until there is one, and a member it
 * leaves out names no test case, so its cell says so instead of waiting.
 */
export function memberCell(
  label: string,
  labelMap: Record<string, number>,
  results: ReadonlyMap<TestCaseResult['test_case_id'], TestCaseResult>,
  placeholderId: number,
): DisplayTestCaseResult {
  const testCaseId = labelMap[label];
  if (testCaseId === undefined) {
    return {
      id: placeholderId,
      test_case_id: placeholderId,
      score: null,
      verdict: null,
      isPlaceholder: true,
      isMissing: true,
      label,
    };
  }
  return (
    results.get(testCaseId) ?? {
      id: placeholderId,
      test_case_id: testCaseId,
      score: 0,
      verdict: 'Pending',
      isPlaceholder: true,
      label,
    }
  );
}
