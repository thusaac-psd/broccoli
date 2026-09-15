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
