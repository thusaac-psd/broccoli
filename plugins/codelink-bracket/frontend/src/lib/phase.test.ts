// Pure-logic test for `describeMatchPhase` (see `phase.ts`), following the
// `node --experimental-strip-types --test` convention.
//
// The one invariant this suite exists to pin: `awaiting_judge` must map to
// a status DISTINCT from `in_progress`/`tiebreak`. A match sitting in
// `MatchPhase::AwaitingJudge` (plugins/codelink-bracket/src/model.rs) means
// a 小局's deadline passed while an older submission was still in flight and
// the server suspects a platform fault -- rendering that the same as
// ordinary "in progress" would hide exactly the state staff most need to
// notice (which submission is blocking, via `awaiting_submission_id`) and
// would tell a player they are "still playing" when the truth is "nobody
// knows the outcome yet, and it is not your move".
import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { MatchPhase } from '../types.ts';
import { describeMatchPhase, isActivelyPlaying } from './phase.ts';

const ALL_PHASES: MatchPhase[] = [
  'pending',
  'ordering',
  'in_progress',
  'tiebreak',
  'awaiting_judge',
  'decided',
  'needs_adjudication',
];

test('every MatchPhase maps to a description with a non-empty label key', () => {
  for (const phase of ALL_PHASES) {
    const description = describeMatchPhase(phase);
    assert.ok(
      description.labelKey.length > 0,
      `phase "${phase}" got an empty label key`,
    );
  }
});

test('awaiting_judge is a distinct kind from in_progress', () => {
  assert.notEqual(
    describeMatchPhase('awaiting_judge').kind,
    describeMatchPhase('in_progress').kind,
  );
});

test('awaiting_judge is a distinct kind from tiebreak', () => {
  assert.notEqual(
    describeMatchPhase('awaiting_judge').kind,
    describeMatchPhase('tiebreak').kind,
  );
});

test('awaiting_judge maps to its own literal kind, not a reused one', () => {
  // Guards against a future refactor quietly folding `awaiting_judge` back
  // into `'playing'` (or any other existing kind) to "simplify" the switch.
  const kinds = ALL_PHASES.filter((p) => p !== 'awaiting_judge').map(
    (p) => describeMatchPhase(p).kind,
  );
  assert.equal(describeMatchPhase('awaiting_judge').kind, 'awaiting_judge');
  assert.ok(!kinds.includes('awaiting_judge'));
});

test('in_progress and tiebreak are both "playing" but each keeps a distinguishing label', () => {
  const inProgress = describeMatchPhase('in_progress');
  const tiebreak = describeMatchPhase('tiebreak');
  assert.equal(inProgress.kind, 'playing');
  assert.equal(tiebreak.kind, 'playing');
  assert.notEqual(inProgress.labelKey, tiebreak.labelKey);
});

test('isActivelyPlaying is true only for in_progress and tiebreak', () => {
  for (const phase of ALL_PHASES) {
    const expected = phase === 'in_progress' || phase === 'tiebreak';
    assert.equal(isActivelyPlaying(phase), expected, `phase "${phase}"`);
  }
});

test('isActivelyPlaying is false for awaiting_judge', () => {
  // Restated as its own test (redundant with the loop above) because this
  // is the specific case a regression is most likely to reintroduce: someone
  // "fixing" a bug where a player can't submit during awaiting_judge by
  // widening this check, defeating the whole point of the phase.
  assert.equal(isActivelyPlaying('awaiting_judge'), false);
});

test('every phase has a short card label, and awaiting_judge keeps its own', () => {
  const shorts = ALL_PHASES.map((p) => describeMatchPhase(p).shortKey);
  for (const key of shorts) assert.ok(key.length > 0);
  assert.notEqual(
    describeMatchPhase('awaiting_judge').shortKey,
    describeMatchPhase('in_progress').shortKey,
  );
});
