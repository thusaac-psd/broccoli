import type { components } from '@/api/schema';

export type SubmissionStatus = components['schemas']['SubmissionStatus'];

export type Submission = components['schemas']['SubmissionResponse'];
/**
 * The shape `POST /admin/submissions/fan-out`, `PATCH .../rejudge`, and
 * judgement-apply endpoints return: every field but `id` is optional. `id`
 * is always present; the rest are present (Allow), present-but-`null`
 * (Redact), or entirely absent from the response object (Deny - the caller's
 * own Read visibility into this submission was denied even though the
 * mutation that produced it went through). See
 * `SubmissionResponseAfterMutation`'s doc comment in
 * `packages/server/src/models/submission.rs` for the full contract.
 */
export type SubmissionAfterMutation =
  components['schemas']['SubmissionResponseAfterMutation'];
export type SubmissionSummary = components['schemas']['SubmissionListItem'];
export type SubmissionJudgement =
  components['schemas']['SubmissionJudgementResponse'];

export type JudgeResult = components['schemas']['JudgeResultResponse'];
export type TestCaseResult = components['schemas']['TestCaseResultResponse'];
export type Verdict = TestCaseResult['verdict'];

export type CodeRun = components['schemas']['CodeRunResponse'];
export type CodeRunJudgeResult = components['schemas']['CodeRunJudgeResult'];
export type CodeRunResult = components['schemas']['CodeRunResultResponse'];

export const SUBMISSION_STATUSES: SubmissionStatus[] = [
  'Pending',
  'Compiling',
  'Running',
  'Judged',
  'CompilationError',
  'SystemError',
];

export type SubmissionStatusFilterValue = 'all' | SubmissionStatus;

/**
 * Error envelope returned by the server for failed submissions and code
 * runs. The `{ code, message, details? }` shape is an API contract shared
 * by all submission endpoints.
 */
export interface SubmissionError {
  code: string;
  message: string;
  details?: Record<string, unknown>;
}

export const SUBMISSION_STATUS_FILTER_OPTIONS: SubmissionStatusFilterValue[] = [
  'all',
  ...SUBMISSION_STATUSES,
];
