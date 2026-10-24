// What the bracket summary bar says: which rounds are being played, why a
// match needs staff, and whether the contest end will cut matches short.

import type { MatchView } from '../types.ts';

/** Count only the current stage, including pairings not created yet. */
export function currentRoundProgress(
  matches: MatchView[],
  roundCount: number,
  thirdPlace: boolean,
) {
  for (let round = 1; round <= roundCount; round++) {
    const planned =
      2 ** (roundCount - round) + Number(thirdPlace && round === roundCount);
    const finished = matches.filter(
      (match) => match.round === round && match.state === 'decided',
    ).length;
    if (finished < planned) return { round, remaining: planned - finished };
  }
  return null;
}

/**
 * Stages with unfinished matches, lowest first. Between stages, report the
 * next unopened stage. Empty once the tournament is decided.
 */
export function roundsInPlay(
  matches: MatchView[],
  roundCount = matches[0]?.stage_count ?? 0,
  thirdPlace = matches[0]?.third_place_enabled ?? false,
): number[] {
  const total = 2 ** roundCount - 1 + Number(thirdPlace);
  const decided = matches.filter((m) => m.state === 'decided').length;
  if (decided === total) return [];
  const rounds = new Set(
    matches.filter((m) => m.state !== 'decided').map((m) => m.round),
  );
  if (rounds.size === 0) {
    // Every created match is decided but later ones do not exist yet: the
    // next round is the one in play.
    const highest = Math.max(0, ...matches.map((m) => m.round));
    rounds.add(Math.min(highest + 1, roundCount));
  }
  return [...rounds].sort((a, b) => a - b);
}

/** Translation key (and params) for why a `needs_adjudication` match needs staff. */
export function adjudicationMessage(match: MatchView): {
  key: string;
  params?: Record<string, number>;
} {
  const id = match.awaiting_submission_id;
  switch (match.adjudication_reason) {
    case 'contest_ended':
      return { key: 'codelink-bracket.match.contestEnded' };
    case 'setup_missing':
      return { key: 'codelink-bracket.match.setupMissing' };
    case 'tiebreak_exhausted':
      return { key: 'codelink-bracket.match.tiebreakExhausted' };
    case 'stuck_judge':
      if (id !== null) {
        return { key: 'codelink-bracket.match.stuckJudge', params: { id } };
      }
      break;
    case null:
    case undefined:
      break;
  }
  // Matches escalated before the server recorded a reason.
  return id !== null
    ? { key: 'codelink-bracket.match.stuckJudge', params: { id } }
    : { key: 'codelink-bracket.match.tiebreakExhausted' };
}

export type ContestEndWarning =
  | { kind: 'ended'; unfinished: number }
  | { kind: 'cutoff'; count: number };

/**
 * Whether the contest end has cut, or is about to cut, matches short. Once
 * it has passed, every unfinished match counts. Before that, a match counts
 * if its running game's deadline, or its scheduled start, lies past the
 * end: those are the ones that will certainly not finish on their own.
 */
export function contestEndWarning(
  matches: MatchView[],
  contestEndMs: number | null | undefined,
  now: number,
  roundCount = matches[0]?.stage_count ?? 0,
  thirdPlace = matches[0]?.third_place_enabled ?? false,
): ContestEndWarning | null {
  if (contestEndMs == null) return null;
  const total = 2 ** roundCount - 1 + Number(thirdPlace);
  const decided = matches.filter((m) => m.state === 'decided').length;
  if (decided === total) return null;
  if (now >= contestEndMs) {
    return { kind: 'ended', unfinished: total - decided };
  }
  const count = matches.filter((m) => {
    if (m.state === 'in_progress' || m.state === 'tiebreak') {
      return (m.current_xiaoju_deadline_ms ?? 0) > contestEndMs;
    }
    if (m.state === 'ordering') {
      return m.starts_at_ms !== null && m.starts_at_ms >= contestEndMs;
    }
    return false;
  }).length;
  return count > 0 ? { kind: 'cutoff', count } : null;
}
