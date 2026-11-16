import assert from 'node:assert/strict';
import { test } from 'node:test';

import { registerPath, safeReturnPath, signInPath } from './sign-in.ts';

test('signing in returns to the page that asked', () => {
  assert.equal(
    signInPath('/contests/1/problems/2?tab=coding'),
    '/login?next=%2Fcontests%2F1%2Fproblems%2F2%3Ftab%3Dcoding',
  );
  assert.equal(signInPath('/'), '/login');
  assert.equal(registerPath('/problems/3'), '/register?next=%2Fproblems%2F3');
});

test('the sign-in page never returns to itself', () => {
  assert.equal(signInPath('/login?next=%2Fx'), '/login');
});

test('only same-origin paths are return targets', () => {
  assert.equal(safeReturnPath('https://evil.example'), '/');
  assert.equal(safeReturnPath('//evil.example/x'), '/');
  assert.equal(safeReturnPath(null), '/');
  assert.equal(safeReturnPath('/problems/1'), '/problems/1');
  assert.equal(signInPath('//evil.example'), '/login');
});
