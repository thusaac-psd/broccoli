// Shared `MatchView` builder for the lib tests. Not imported by app code.

import type { MatchView } from '../types.ts';

export function makeMatch(overrides: Partial<MatchView> = {}): MatchView {
  return {
    id: 0,
    round: 1,
    pos: 0,
    player_a: 10,
    player_b: 20,
    player_a_name: 'alice',
    player_b_name: 'bob',
    group_a: [101, 102, 103],
    group_b: [201, 202, 203],
    order_a: null,
    order_b: null,
    tiebreak_problem: null,
    score_a: 0,
    score_b: 0,
    state: 'ordering',
    winner: null,
    decided_at_ms: 0,
    awaiting_submission_id: null,
    adjudication_reason: null,
    current_xiaoju_index: null,
    current_xiaoju_deadline_ms: null,
    current_xiaoju_opened_at_ms: null,
    games: [],
    starts_at_ms: null,
    ...overrides,
  };
}
