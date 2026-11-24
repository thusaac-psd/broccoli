import assert from 'node:assert/strict';
import { test } from 'node:test';

import { CheckCircle2, Clock, MinusCircle, XCircle } from 'lucide-react';

import { verdictStyle } from './verdict-style.ts';

test('each verdict has its own style, the same wherever a test case shows', () => {
  assert.equal(verdictStyle('Accepted', 'Judged').icon, CheckCircle2);
  assert.match(verdictStyle('Accepted', 'Judged').text, /green/);
  assert.equal(verdictStyle('WrongAnswer', 'Judged').icon, XCircle);
  assert.match(verdictStyle('WrongAnswer', 'Judged').text, /red/);
  assert.equal(verdictStyle('Cancelled', 'Judged').icon, MinusCircle);
  assert.notEqual(
    verdictStyle('SomePluginVerdict', 'Judged'),
    verdictStyle('SystemError', 'Judged'),
  );
});

test('a verdict a mask withheld reads as skipped once judged, as pending before', () => {
  assert.equal(verdictStyle(null, 'Judged'), verdictStyle('Skipped', 'Judged'));
  assert.equal(
    verdictStyle(null, 'Running'),
    verdictStyle('Skipped', 'Running'),
  );
  assert.equal(verdictStyle(null, 'Queued').icon, Clock);
});
