import type { UserDisplayIdentity } from '@broccoli/web-sdk/utils';

export type QualificationVerdict = 'pending' | 'qualified';

export type ProblemCell = {
  submission_id: number;
  time_seconds: number;
} & (
  | { status: 'credited'; slot: number }
  | { status: 'slots_full' | 'after_qualification'; slot: null }
);

export type CreditStatus = ProblemCell['status'];

export interface Standing extends UserDisplayIdentity {
  user_id: number;
  /** Number of credited problems, independent of their point values. */
  credited: number;
  score: number;
  accepted: number;
  qualification_verdict: QualificationVerdict | null;
  qualified_at_seconds: number | null;
  problems: Record<string, ProblemCell>;
}

export interface ProblemSlots {
  problem_id: number;
  label: string;
  points: number;
  remaining: number;
  awards: (UserDisplayIdentity & {
    user_id: number;
    submission_id: number;
    time_seconds: number;
  })[];
}

export interface ContestInfoResponse {
  phase: 'before' | 'during' | 'after';
  problem_count: number;
  expected_problem_count: number;
  slots_per_problem: number;
  solves_to_qualify: number;
  advancement_slots: number;
  scoreboard_refresh_seconds: number;
}

export interface StandingsResponse extends ContestInfoResponse {
  qualified_count: number;
  pending_submissions: number;
  problems: ProblemSlots[];
  rows: Standing[];
  /** Available after the contest ends and all official judging finishes. */
  advancement: {
    direct_user_ids: number[];
    reserve_user_ids: number[];
    unfilled_slots: number;
  } | null;
}
