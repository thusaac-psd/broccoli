// Pure mapping from `MatchPhase` (the server's authoritative match
// lifecycle state -- see plugins/codelink-bracket/src/model.rs) to what a
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
  /** Translation key for a one- or two-word form, for bracket cards. */
  shortKey: string;
}

export function describeMatchPhase(phase: MatchPhase): MatchStatusDescription {
  switch (phase) {
    case 'pending':
      return {
        kind: 'not_started',
        labelKey: 'codelink-bracket.phase.pending',
        shortKey: 'codelink-bracket.phase.short.pending',
      };
    case 'ordering':
      return {
        kind: 'ordering',
        labelKey: 'codelink-bracket.phase.ordering',
        shortKey: 'codelink-bracket.phase.short.ordering',
      };
    case 'in_progress':
      return {
        kind: 'playing',
        labelKey: 'codelink-bracket.phase.inProgress',
        shortKey: 'codelink-bracket.phase.short.live',
      };
    case 'tiebreak':
      return {
        kind: 'playing',
        labelKey: 'codelink-bracket.phase.tiebreak',
        shortKey: 'codelink-bracket.phase.short.tiebreak',
      };
    case 'awaiting_judge':
      return {
        kind: 'awaiting_judge',
        labelKey: 'codelink-bracket.phase.awaitingJudge',
        shortKey: 'codelink-bracket.phase.short.judging',
      };
    case 'decided':
      return {
        kind: 'decided',
        labelKey: 'codelink-bracket.phase.decided',
        shortKey: 'codelink-bracket.phase.short.decided',
      };
    case 'needs_adjudication':
      return {
        kind: 'needs_adjudication',
        labelKey: 'codelink-bracket.phase.needsAdjudication',
        shortKey: 'codelink-bracket.phase.short.staff',
      };
  }
}

/**
 * Whether `phase` is one where a player may currently be submitting to a
 * live 小局 (`before_submission`'s own gate is the real authority -- see
 * plugins/codelink-bracket/src/gate.rs -- this only drives whether the UI
 * should even attempt to show live-submission affordances).
 */
export function isActivelyPlaying(phase: MatchPhase): boolean {
  return phase === 'in_progress' || phase === 'tiebreak';
}
