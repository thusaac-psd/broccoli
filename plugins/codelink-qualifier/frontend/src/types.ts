export type QualificationVerdict = 'pending' | 'qualified';

export type ProblemCell = {
  submission_id: number;
  time_seconds: number;
} & (
  | { status: 'credited'; slot: number }
  | { status: 'slots_full' | 'after_qualification'; slot: null }
);

export type CreditStatus = ProblemCell['status'];

export interface Standing {
  user_id: number;
  username: string;
  credited: number;
  accepted: number;
  qualification_verdict: QualificationVerdict | null;
  qualified_at_seconds: number | null;
  problems: Record<string, ProblemCell>;
}

export interface ProblemSlots {
  problem_id: number;
  label: string;
  remaining: number;
  awards: {
    user_id: number;
    username: string;
    submission_id: number;
    time_seconds: number;
  }[];
}

export interface ContestInfoResponse {
  phase: 'before' | 'during' | 'after';
  problem_count: number;
  expected_problem_count: number;
  slots_per_problem: number;
  solves_to_qualify: number;
  scoreboard_refresh_seconds: number;
}

export interface StandingsResponse extends ContestInfoResponse {
  qualified_count: number;
  pending_submissions: number;
  problems: ProblemSlots[];
  rows: Standing[];
}
