import assert from 'node:assert/strict';
import { test } from 'node:test';
import { matchesInRound, roundNameKey } from './rounds.ts';

test('stage names and widths follow bracket size', () => {
  for (const stages of [1, 3, 4, 5, 7]) {
    assert.equal(matchesInRound(1, stages), 2 ** (stages - 1));
    assert.equal(matchesInRound(stages, stages), 1);
    assert.equal(roundNameKey(stages, stages), 'codelink-bracket.round.final');
  }
  assert.equal(roundNameKey(1, 3), 'codelink-bracket.round.quarterfinal');
  assert.equal(roundNameKey(1, 5), 'codelink-bracket.round.of32');
});
