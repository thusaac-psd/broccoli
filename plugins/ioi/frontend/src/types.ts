import type { UserDisplayIdentity } from '@broccoli/web-sdk/utils';

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
  /** The problem's maximum score, the sum of its subtasks' maximums. */
  max_score: number;
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
  /**
   * The viewer's submissions a token can be spent on now, newest first:
   * judged and not yet tokened. A submission's own status cannot say so,
   * since feedback may withhold it from its owner.
   */
  usable_submissions: { submission_id: number; problem_id: number }[];
}

export interface UseTokenResponse {
  remaining_tokens: number;
}

export interface SubtaskScoreEntry {
  name: string;
  scoring_method: SubtaskScoringMethod;
  score: number;
  max_score: number;
}

export interface SubtaskScoresResponse {
  subtasks: SubtaskScoreEntry[] | null;
  /** The maximum the judgement was scored against; `null` until it stands, or when the viewer reads no total. */
  max_score: number | null;
}

export interface ScoreboardProblemScore {
  problem_id: number;
  score: number;
}

export interface ScoreboardEntry extends UserDisplayIdentity {
  /** `null` when the board shows only the viewer's own row, or no scores. */
  rank: number | null;
  user_id: number;
  /** Absent, with `total_time_seconds`, when the viewer's feedback level shows no score. */
  total_score?: number;
  total_time_seconds?: number;
  problems?: ScoreboardProblemScore[];
}

export interface ScoreboardResponse {
  phase: 'before' | 'during' | 'after';
  /** Whether the board may still change: the contest runs or a submission is still being judged. */
  live: boolean;
  scoring_mode: ScoringMode;
  feedback_level: FeedbackLevel;
  scoreboard_visibility: ScoreboardVisibility;
  scoreboard_tiebreaker: ScoreboardTiebreaker;
  max_scores: Record<string, number>;
  /** The contest's problems in its order, with the labels the contest gives them. */
  problems: { problem_id: number; label: string }[];
  /** False when `rankings` holds only the viewer's own row, or nothing. */
  full_board: boolean;
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
