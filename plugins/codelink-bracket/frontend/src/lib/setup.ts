// Bracket setup rules for the staff setup screen. Mirrors the server's
// `validate_setup` (src/setup.rs) so the form can say what is wrong before
// submitting; the server stays the authority.

import { ROUND_COUNT } from './rounds.ts';

export const PLAYER_COUNT = 16;

/** A round's timing and problem slots while editing; `null` is an empty slot. */
export interface RoundDraft {
  groupA: (number | null)[];
  groupB: (number | null)[];
  tiebreak: (number | null)[];
  gameMinutes: number;
  tiebreakMinutes: number;
  breakMinutes: number;
}

export const emptyRound = (index = 0): RoundDraft => ({
  groupA: [null, null, null],
  groupB: [null, null, null],
  tiebreak: [null],
  gameMinutes: 30,
  tiebreakMinutes: 30,
  breakMinutes: index === 0 ? 0 : 10,
});

/**
 * Fill every round from the contest's problem order: three for the first
 * player, three for the second, one tiebreak, round after round. Slots past
 * the end of the list stay empty.
 */
export function autoFillRounds(
  problemIds: number[],
  currentRounds?: RoundDraft[],
): RoundDraft[] {
  const queue = [...problemIds];
  const take = () => queue.shift() ?? null;
  return Array.from({ length: ROUND_COUNT }, (_, i) => ({
    ...(currentRounds?.[i] ?? emptyRound(i)),
    groupA: [take(), take(), take()],
    groupB: [take(), take(), take()],
    tiebreak: [take()],
  }));
}

export type SetupIssue =
  | { kind: 'players'; count: number }
  | { kind: 'emptySlot'; round: number }
  | { kind: 'duplicate'; problemId: number }
  | { kind: 'badTiming'; round: number }
  | { kind: 'badPenalty' }
  | { kind: 'badGrace' };

export function validateSetup(
  seeds: number[],
  rounds: RoundDraft[],
  penaltySeconds: number,
  graceSeconds: number,
): SetupIssue[] {
  const issues: SetupIssue[] = [];
  if (new Set(seeds).size !== PLAYER_COUNT || seeds.length !== PLAYER_COUNT) {
    issues.push({ kind: 'players', count: new Set(seeds).size });
  }
  const seen = new Set<number>();
  const dupes = new Set<number>();
  rounds.forEach((r, i) => {
    const validSeconds = (minutes: number, minimum: number) => {
      const seconds = Math.round(minutes * 60);
      return Number.isSafeInteger(seconds) && seconds >= minimum;
    };
    if (
      !validSeconds(r.gameMinutes, 1) ||
      !validSeconds(r.tiebreakMinutes, 1) ||
      !validSeconds(r.breakMinutes, 0) ||
      r.breakMinutes < 0 ||
      (i === 0 && r.breakMinutes !== 0)
    ) {
      issues.push({ kind: 'badTiming', round: i + 1 });
    }
    const regular = [...r.groupA, ...r.groupB];
    const tiebreaks = r.tiebreak.filter((id): id is number => id !== null);
    if (regular.some((id) => id === null) || tiebreaks.length === 0) {
      issues.push({ kind: 'emptySlot', round: i + 1 });
    }
    for (const id of [...regular, ...tiebreaks]) {
      if (id === null) continue;
      if (seen.has(id)) dupes.add(id);
      seen.add(id);
    }
  });
  for (const problemId of dupes) issues.push({ kind: 'duplicate', problemId });
  if (!Number.isSafeInteger(penaltySeconds) || penaltySeconds < 0) {
    issues.push({ kind: 'badPenalty' });
  }
  if (!Number.isSafeInteger(graceSeconds) || graceSeconds < 0) {
    issues.push({ kind: 'badGrace' });
  }
  return issues;
}
