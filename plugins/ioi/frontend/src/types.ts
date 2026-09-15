import type { TestCaseResult } from '@broccoli/web-sdk/submission';

export interface ContestInfoResponse {
  scoring_mode: ScoringMode;
  feedback_level: FeedbackLevel;
  scoreboard_visibility: ScoreboardVisibility;
  scoreboard_tiebreaker: ScoreboardTiebreaker;
  token_mode: TokenMode;
}

export interface SubtaskInfo {
  name: string;
  scoring_method: SubtaskScoringMethod;
  max_score: number;
  /** Present when full testcase mapping is available for this viewer. */
  test_cases?: string[];
}

export interface TaskConfigResponse {
  scoring_mode: ScoringMode;
  feedback_level: FeedbackLevel;
  subtasks?: SubtaskInfo[];
  /** Maps test case label -\> test_case_id when full testcase mapping is available. */
  label_map?: Record<string, number>;
  /** Maps test case label -\> max score when full testcase mapping is available. */
  test_case_max_scores?: Record<string, number>;
}

export interface TokenStatusResponse {
  mode: TokenMode;
  available: number;
  used: number;
  total: number;
  next_regen_at?: string | null;
  tokened_submission_ids: number[];
}

export interface UseTokenResponse {
  remaining_tokens: number;
  task_score: number;
}

export interface SubtaskScoreEntry {
  name: string;
  scoring_method: SubtaskScoringMethod;
  score: number;
  max_score: number;
}

export interface SubtaskScoresResponse {
  subtasks: SubtaskScoreEntry[] | null;
}

export interface ScoreboardProblemScore {
  problem_id: number;
  score: number;
}

export interface ScoreboardEntry {
  rank: number;
  user_id: number;
  username: string;
  total_score: number;
  total_time_seconds: number;
  problems?: ScoreboardProblemScore[];
}

export interface ScoreboardResponse {
  phase: 'before' | 'during' | 'after';
  scoring_mode: ScoringMode;
  feedback_level: FeedbackLevel;
  scoreboard_visibility: ScoreboardVisibility;
  scoreboard_tiebreaker: ScoreboardTiebreaker;
  max_scores: Record<string, number>;
  rankings: ScoreboardEntry[];
}

export interface SubmissionStatusResponse {
  last_submission_verdict: string | null;
  last_submission_score: number | null;
}

export type ScoringMode =
  | 'max_submission'
  | 'sum_best_subtask'
  | 'best_tokened_or_last';
export type FeedbackLevel = 'full' | 'subtask_scores' | 'total_only' | 'none';
export type ScoreboardVisibility = 'admins_only' | 'all_contest_viewers';
export type ScoreboardTiebreaker =
  | 'equal_rank'
  | 'sum_score_time'
  | 'max_score_time';
export type TokenMode = 'none' | 'fixed_budget' | 'regenerating';
export type SubtaskScoringMethod = 'group_min' | 'sum' | 'group_mul';

/**
 * Wire shape of a single test-case result once the visibility kernel's field
 * mask has been applied. The `subtask_scores`/`total_only` feedback levels
 * NULL a masked test case's `verdict`/`score` (see plugins/ioi/src/
 * feedback.rs's `per_test_case_mask_fields`) rather than authoring a
 * `"Skipped"`/`0` value the way the old host-fn-based redaction did -- a
 * field mask can only blank a value, never author one.
 *
 * `TestCaseResult` (generated from the OpenAPI schema) still declares both
 * fields non-nullable: the schema does not model this post-mask wire shape.
 * This is an approved, intentional wire/schema divergence -- see
 * `normalizeMaskedTestCase` below, which is how this plugin's own frontend
 * compensates at render time so what the contestant sees is unchanged.
 */
export type MaskedTestCaseResult = Omit<TestCaseResult, 'verdict' | 'score'> & {
  verdict: TestCaseResult['verdict'] | null;
  score: TestCaseResult['score'] | null;
};

/**
 * Restore the pre-refactor RENDERED equivalence for a single test case:
 * a masked (null) verdict renders exactly like the old redaction's
 * authored `"Skipped"` value; a masked (null) score renders as `0`.
 * Equivalence is defined at the rendered level, not the wire level -- do
 * not "fix" a null here by asking the backend to author `"Skipped"`/`0`
 * again, which would reintroduce a write-capable decision variant.
 */
export function normalizeMaskedTestCase(
  tc: MaskedTestCaseResult,
): TestCaseResult {
  return {
    ...tc,
    verdict: tc.verdict ?? 'Skipped',
    score: tc.score ?? 0,
  } as TestCaseResult;
}
