// Pure mapping from `MatchPhase` (the server's authoritative match
// lifecycle state -- see plugins/afternoon-bracket/src/model.rs) to what a
// viewer is told. Exists as its own module, independent of any component,
// so the one invariant that matters most can be unit-tested directly: a
// match in `awaiting_judge` must NEVER be described the same way as one in
// `in_progress`/`tiebreak`. Getting this backwards would show a player (or
// staff) an ordinary "still playing" status while a stuck submission is
// silently blocking the match -- exactly the failure mode
// `MatchPhase::AwaitingJudge`'s doc comment exists to prevent.

import type { MatchPhase } from '../types.ts';

/**
 * A coarser grouping than `MatchPhase` for UI branching (e.g. "should the
 * live-play controls render"), but `'awaiting_judge'` is kept as its OWN
 * kind rather than folded into `'playing'` -- see the module doc comment.
 */
export type MatchStatusKind =
  | 'not_started'
  | 'ordering'
  | 'playing'
  | 'awaiting_judge'
  | 'decided'
  | 'needs_adjudication';

export interface MatchStatusDescription {
  kind: MatchStatusKind;
  /** Translation key for a short summary (`i18n/*.toml` in this plugin). */
  labelKey: string;
}

export function describeMatchPhase(phase: MatchPhase): MatchStatusDescription {
  switch (phase) {
    case 'pending':
      return {
        kind: 'not_started',
        labelKey: 'afternoon-bracket.phase.pending',
      };
    case 'ordering':
      return { kind: 'ordering', labelKey: 'afternoon-bracket.phase.ordering' };
    case 'in_progress':
      return {
        kind: 'playing',
        labelKey: 'afternoon-bracket.phase.inProgress',
      };
    case 'tiebreak':
      return { kind: 'playing', labelKey: 'afternoon-bracket.phase.tiebreak' };
    case 'awaiting_judge':
      return {
        kind: 'awaiting_judge',
        labelKey: 'afternoon-bracket.phase.awaitingJudge',
      };
    case 'decided':
      return { kind: 'decided', labelKey: 'afternoon-bracket.phase.decided' };
    case 'needs_adjudication':
      return {
        kind: 'needs_adjudication',
        labelKey: 'afternoon-bracket.phase.needsAdjudication',
      };
  }
}

/**
 * Whether `phase` is one where a player may currently be submitting to a
 * live 小局 (`before_submission`'s own gate is the real authority -- see
 * plugins/afternoon-bracket/src/gate.rs -- this only drives whether the UI
 * should even attempt to show live-submission affordances).
 */
export function isActivelyPlaying(phase: MatchPhase): boolean {
  return phase === 'in_progress' || phase === 'tiebreak';
}
