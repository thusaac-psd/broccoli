// What the bracket summary bar says: which rounds are being played, why a
// match needs staff, and whether the contest end will cut matches short.

import type { MatchView } from '../types.ts';
import { ROUND_COUNT } from './rounds.ts';

export const MATCH_COUNT = 15;

/**
 * The rounds with a match still to finish, lowest first. Matches start per
 * pair, so a quarterfinal can be live while round-of-16 games still run;
 * naming only the lowest round would hide that. Empty once every match is
 * decided.
 */
export function roundsInPlay(matches: MatchView[]): number[] {
  const decided = matches.filter((m) => m.state === 'decided').length;
  if (decided === MATCH_COUNT) return [];
  const rounds = new Set(
    matches.filter((m) => m.state !== 'decided').map((m) => m.round),
  );
  if (rounds.size === 0) {
    // Every created match is decided but later ones do not exist yet: the
    // next round is the one in play.
    const highest = Math.max(0, ...matches.map((m) => m.round));
    rounds.add(Math.min(highest + 1, ROUND_COUNT));
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
): ContestEndWarning | null {
  if (contestEndMs == null) return null;
  const decided = matches.filter((m) => m.state === 'decided').length;
  if (decided === MATCH_COUNT) return null;
  if (now >= contestEndMs) {
    return { kind: 'ended', unfinished: MATCH_COUNT - decided };
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
