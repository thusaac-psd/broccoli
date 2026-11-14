import type { Problem } from '@broccoli/web-sdk/problem';

import type { MessageKey } from '@/lib/i18n/catalog';

/** What the problem create and edit forms hold. */
export interface ProblemFormData {
  title: string;
  content: string;
  timeLimit: number;
  memoryLimit: number;
  problemType: string;
  checkerFormat: string;
  defaultContestType: string;
  showTestDetails: boolean;
  isPublic: boolean;
  submissionFormat: Record<string, string[]>;
}

export const NEW_PROBLEM: ProblemFormData = {
  title: '',
  content: '',
  timeLimit: 1000,
  memoryLimit: 262144,
  problemType: '',
  // The server's default once the registries load (see checkerFormatFor).
  checkerFormat: '',
  defaultContestType: '',
  showTestDetails: false,
  isPublic: false,
  submissionFormat: {},
};

export function problemFormValues(problem: Problem): ProblemFormData {
  return {
    title: problem.title,
    content: problem.content,
    timeLimit: problem.time_limit,
    memoryLimit: problem.memory_limit,
    problemType: problem.problem_type,
    checkerFormat: problem.checker_format,
    defaultContestType: problem.default_contest_type,
    showTestDetails: problem.show_test_details,
    isPublic: problem.is_public,
    submissionFormat: problem.submission_format ?? {},
  };
}

/**
 * The checker format the form shows: the chosen one while it is registered,
 * otherwise the server's default (`default_checker_format` of the
 * registries), otherwise the first registered format.
 */
export function checkerFormatFor(
  current: string,
  registered: readonly string[],
  serverDefault: string | undefined,
): string {
  if (registered.includes(current)) return current;
  if (serverDefault && registered.includes(serverDefault)) return serverDefault;
  return registered[0] ?? '';
}

/** The i18n key of the first problem with `data`, or null when it is valid. */
export function problemFormProblem(data: ProblemFormData): MessageKey | null {
  if (!data.title.trim()) return 'validation.titleRequired';
  if (!data.content.trim()) return 'validation.contentRequired';
  if (!data.problemType) return 'validation.problemTypeRequired';
  if (!data.checkerFormat) return 'validation.checkerFormatRequired';
  if (!data.defaultContestType) return 'validation.contestTypeRequired';
  return null;
}

/** The create and update request body for `data`. */
export function problemRequestBody(data: ProblemFormData) {
  return {
    title: data.title,
    content: data.content,
    time_limit: data.timeLimit,
    memory_limit: data.memoryLimit,
    problem_type: data.problemType,
    checker_format: data.checkerFormat,
    default_contest_type: data.defaultContestType,
    show_test_details: data.showTestDetails,
    is_public: data.isPublic,
    submission_format:
      Object.keys(data.submissionFormat).length > 0
        ? data.submissionFormat
        : null,
  };
}
