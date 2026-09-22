// Pure-logic test for `isPermutationOf`/`canSubmitOrder` (see `ordering.ts`),
// following the `node --experimental-strip-types --test` convention used by
// packages/web/src/**/*.test.ts and plugins/print/web/src/lib/*.test.ts: no
// bundler/DOM needed since the functions under test are pure.
//
// This suite exists to lock in the UI-side half of the permutation
// invariant the task calls out explicitly: the backend's `record_order`
// (plugins/afternoon-bracket/src/ordering.rs) already rejects a
// non-permutation with a 400, but a UI that lets a player click "submit" on
// a ranking that is doomed to be rejected is a bug in its own right. These
// tests pin `isPermutationOf` to the exact same algorithm as the Rust
// `is_permutation_of` it mirrors (sort-and-compare), and `canSubmitOrder` to
// gating on BOTH "is a permutation" and "the opponent's group is fully
// visible" (a masked entry must not be treated as absent-but-fine).
import assert from 'node:assert/strict';
import { test } from 'node:test';

import { canSubmitOrder, isPermutationOf } from './ordering.ts';

test('a reordering of the same 3 values is a valid permutation', () => {
  assert.equal(isPermutationOf([103, 101, 102], [101, 102, 103]), true);
});

test('the identity ordering is a valid permutation', () => {
  assert.equal(isPermutationOf([101, 102, 103], [101, 102, 103]), true);
});

test('a duplicate in place of a missing value is rejected', () => {
  // Repeats problem 101 instead of including 103 -- same length, but not a
  // permutation. This is the case a naive "are all 3 group ids present as a
  // SET in order" check without a matching multiplicity check could miss if
  // it deduplicated first.
  assert.equal(isPermutationOf([101, 101, 102], [101, 102, 103]), false);
});

test('a foreign id not in the group at all is rejected', () => {
  assert.equal(isPermutationOf([101, 102, 999], [101, 102, 103]), false);
});

test('an order shorter than the group is rejected', () => {
  assert.equal(isPermutationOf([101, 102], [101, 102, 103]), false);
});

test('an order longer than the group is rejected', () => {
  assert.equal(isPermutationOf([101, 102, 103, 104], [101, 102, 103]), false);
});

test('an empty order against an empty group is trivially a permutation', () => {
  // Negative control for the length-mismatch checks above: this confirms
  // the length check itself does not reject legitimate equal-length (here,
  // zero-length) inputs.
  assert.equal(isPermutationOf([], []), true);
});

test('canSubmitOrder accepts a complete, valid ranking of a fully visible group', () => {
  assert.equal(canSubmitOrder([103, 101, 102], [101, 102, 103]), true);
});

test('canSubmitOrder rejects a partial ranking (player has picked only 2 of 3)', () => {
  assert.equal(canSubmitOrder([101, 102], [101, 102, 103]), false);
});

test('canSubmitOrder rejects when the opponent group is not fully visible', () => {
  // A masked entry (`null`) must not be silently treated as "nothing to
  // rank" -- there is no valid ranking to submit at all while any entry is
  // hidden, even if the player's local `order` happens to be a permutation
  // of the VISIBLE entries.
  assert.equal(canSubmitOrder([101, 102, 103], [101, 102, null]), false);
});

test('canSubmitOrder rejects a non-permutation even when the group is fully visible', () => {
  assert.equal(canSubmitOrder([101, 101, 103], [101, 102, 103]), false);
});
