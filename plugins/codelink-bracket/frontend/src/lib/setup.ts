// Bracket setup rules for the staff setup screen. Mirrors the server's
// `validate_setup` (src/setup.rs) so the form can say what is wrong before
// submitting; the server stays the authority.

import { DEFAULT_PLAYER_COUNT } from './rounds.ts';

export const PLAYER_COUNT = DEFAULT_PLAYER_COUNT;

/** A round's timing and problem slots while editing; `null` is an empty slot. */
export interface RoundDraft {
  groupA: (number | null)[];
  groupB: (number | null)[];
  tiebreak: (number | null)[];
  winsToAdvance: number;
  startAt: string;
  postOrderWaitSeconds: number;
  gameBreakSeconds: number;
  extraMinutes: number;
  gameMinutes: number;
  tiebreakMinutes: number;
  breakMinutes: number;
}

export const emptyRound = (index = 0): RoundDraft => ({
  groupA: [null, null, null],
  groupB: [null, null, null],
  tiebreak: [null],
  winsToAdvance: 2,
  startAt: '',
  postOrderWaitSeconds: 30,
  gameBreakSeconds: 60,
  extraMinutes: 0,
  gameMinutes: 30,
  tiebreakMinutes: 30,
  breakMinutes: index === 0 ? 0 : 10,
});

export type ThirdPlaceDraft =
  | { mode: 'shared' }
  | ({ mode: 'custom' } & Pick<RoundDraft, 'groupA' | 'groupB' | 'tiebreak'>)
  | null;

export function localDateTime(at: number | null | undefined): string {
  if (at == null || !Number.isFinite(at)) return '';
  const date = new Date(at);
  return new Date(at - date.getTimezoneOffset() * 60_000)
    .toISOString()
    .slice(0, -1);
}

export function resizeSlots(
  slots: (number | null)[],
  count: number,
): (number | null)[] {
  return Array.from({ length: count }, (_, i) => slots[i] ?? null);
}

/**
 * Fill the configured slots in each unopened stage from the contest's
 * problem order. Slots past the end of the list stay empty.
 */
export function autoFillRounds(
  problemIds: number[],
  currentRounds?: RoundDraft[],
  stageCount = Math.log2(DEFAULT_PLAYER_COUNT),
  openedRounds = 0,
): RoundDraft[] {
  const frozen = new Set(
    currentRounds
      ?.slice(0, openedRounds)
      .flatMap((r) => [...r.groupA, ...r.groupB, ...r.tiebreak]),
  );
  const queue = problemIds.filter((id) => !frozen.has(id));
  const take = () => queue.shift() ?? null;
  return Array.from({ length: stageCount }, (_, i) => {
    const round = currentRounds?.[i] ?? emptyRound(i);
    if (i < openedRounds) return round;
    return {
      ...round,
      groupA: round.groupA.map(take),
      groupB: round.groupB.map(take),
      tiebreak: round.tiebreak.map(take),
    };
  });
}

export type SetupIssue =
  | { kind: 'players'; count: number }
  | { kind: 'emptySlot'; round: number }
  | { kind: 'duplicate'; problemId: number }
  | { kind: 'badTiming'; round: number }
  | { kind: 'badWinTarget'; round: number }
  | { kind: 'badSchedule'; round: number }
  | { kind: 'badThirdPlace' }
  | { kind: 'badPenalty' }
  | { kind: 'badExtra' }
  | { kind: 'badGrace' };

export function validateSetup(
  seeds: number[],
  rounds: RoundDraft[],
  penaltySeconds: number,
  graceSeconds: number,
  extraMinutes = 0,
  playerCount = DEFAULT_PLAYER_COUNT,
  options: { scheduleFrom?: number; thirdPlace?: ThirdPlaceDraft } = {},
): SetupIssue[] {
  const issues: SetupIssue[] = [];
  if (
    new Set(seeds).size !== playerCount ||
    seeds.length !== playerCount ||
    playerCount < 2 ||
    playerCount > 128 ||
    !Number.isInteger(Math.log2(playerCount)) ||
    rounds.length !== Math.log2(playerCount)
  ) {
    issues.push({ kind: 'players', count: new Set(seeds).size });
  }
  const seen = new Set<number>();
  const dupes = new Set<number>();
  let previousStart: number | null = null;
  rounds.forEach((r, i) => {
    if (r.startAt) {
      const at = Date.parse(r.startAt);
      if (
        !Number.isFinite(at) ||
        (previousStart !== null && at <= previousStart)
      ) {
        issues.push({ kind: 'badSchedule', round: i + 1 });
      }
      previousStart = at;
    } else if (
      options.scheduleFrom !== undefined &&
      i >= options.scheduleFrom
    ) {
      issues.push({ kind: 'badSchedule', round: i + 1 });
    }
    if (
      !Number.isSafeInteger(r.winsToAdvance) ||
      r.winsToAdvance < 1 ||
      r.winsToAdvance > r.groupA.length
    ) {
      issues.push({ kind: 'badWinTarget', round: i + 1 });
    }
    const validSeconds = (minutes: number, minimum: number) => {
      const seconds = Math.round(minutes * 60);
      return Number.isSafeInteger(seconds) && seconds >= minimum;
    };
    if (
      !validSeconds(r.gameMinutes, 1) ||
      !validSeconds(r.tiebreakMinutes, 1) ||
      !validSeconds(r.breakMinutes, 0) ||
      !validSeconds(r.extraMinutes, 0) ||
      !Number.isSafeInteger(r.postOrderWaitSeconds) ||
      r.postOrderWaitSeconds < 0 ||
      !Number.isSafeInteger(r.gameBreakSeconds) ||
      r.gameBreakSeconds < 0 ||
      r.breakMinutes < 0 ||
      (i === 0 && r.breakMinutes !== 0)
    ) {
      issues.push({ kind: 'badTiming', round: i + 1 });
    }
    const regular = [...r.groupA, ...r.groupB];
    const tiebreaks = r.tiebreak.filter((id): id is number => id !== null);
    if (
      !r.groupA.length ||
      r.groupA.length !== r.groupB.length ||
      r.groupA.length + r.tiebreak.length > 256 ||
      regular.some((id) => id === null) ||
      tiebreaks.length === 0
    ) {
      issues.push({ kind: 'emptySlot', round: i + 1 });
    }
    for (const id of [...regular, ...tiebreaks]) {
      if (id === null) continue;
      if (seen.has(id)) dupes.add(id);
      seen.add(id);
    }
  });
  if (options.thirdPlace && playerCount < 4)
    issues.push({ kind: 'badThirdPlace' });
  if (options.thirdPlace?.mode === 'custom') {
    const third = options.thirdPlace;
    const count = rounds.at(-1)?.groupA.length ?? 0;
    const ids = [
      ...third.groupA,
      ...third.groupB,
      ...third.tiebreak.filter((id): id is number => id !== null),
    ];
    if (
      third.groupA.length !== count ||
      third.groupB.length !== count ||
      third.groupA.some((id) => id === null) ||
      third.groupB.some((id) => id === null) ||
      !third.tiebreak.some((id) => id !== null) ||
      count + third.tiebreak.length > 256
    ) {
      issues.push({ kind: 'badThirdPlace' });
    }
    for (const id of ids) {
      if (id === null) continue;
      if (seen.has(id)) dupes.add(id);
      seen.add(id);
    }
  }
  for (const problemId of dupes) issues.push({ kind: 'duplicate', problemId });
  if (!Number.isSafeInteger(penaltySeconds) || penaltySeconds < 0) {
    issues.push({ kind: 'badPenalty' });
  }
  if (!Number.isSafeInteger(graceSeconds) || graceSeconds < 0) {
    issues.push({ kind: 'badGrace' });
  }
  if (
    !Number.isSafeInteger(Math.round(extraMinutes * 60)) ||
    extraMinutes < 0
  ) {
    issues.push({ kind: 'badExtra' });
  }
  return issues;
}
