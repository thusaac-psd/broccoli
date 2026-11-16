import type { UserDisplayIdentity } from '@broccoli/web-sdk/utils';

export interface IcpcContestInfoResponse {
  penalty_minutes: number;
  count_compile_error: boolean;
  show_test_details: boolean;
}

export interface ProblemCell {
  attempts: number;
  solved: boolean;
  time?: number;
  penalty?: number;
  first_solve?: boolean;
  /** Submissions made during the freeze on a not-yet-solved problem (verdict hidden). */
  pending?: number;
}

export interface StandingsEntry extends UserDisplayIdentity {
  rank: number;
  user_id: number;
  solved: number;
  penalty: number;
  problems: Record<string, ProblemCell>;
}

export interface StandingsResponse {
  phase: 'before' | 'during' | 'after';
  /** True when the contestant view is currently frozen (final N minutes). */
  frozen?: boolean;
  /** True once an organizer has manually revealed the final board. */
  revealed?: boolean;
  /** True for organizers while a freeze is in effect / pending reveal. */
  can_reveal?: boolean;
  penalty_minutes: number;
  problem_labels: string[];
  rows: StandingsEntry[];
}
