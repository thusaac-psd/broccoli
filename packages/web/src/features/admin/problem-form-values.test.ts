import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  checkerFormatFor,
  NEW_PROBLEM,
  problemFormProblem,
  problemRequestBody,
} from './problem-form-values.ts';

const complete = {
  ...NEW_PROBLEM,
  title: 'A + B',
  content: 'Add two numbers.',
  problemType: 'standard',
  checkerFormat: 'exact',
  defaultContestType: 'icpc',
};

test('problemFormProblem accepts a complete problem', () => {
  assert.equal(problemFormProblem(complete), null);
});

test('problemFormProblem names the first missing field', () => {
  assert.equal(
    problemFormProblem({ ...complete, title: '  ' }),
    'validation.titleRequired',
  );
  assert.equal(
    problemFormProblem({ ...complete, content: '' }),
    'validation.contentRequired',
  );
  assert.equal(
    problemFormProblem({ ...complete, problemType: '' }),
    'validation.problemTypeRequired',
  );
  assert.equal(
    problemFormProblem({ ...complete, checkerFormat: '' }),
    'validation.checkerFormatRequired',
  );
  assert.equal(
    problemFormProblem({ ...complete, defaultContestType: '' }),
    'validation.contestTypeRequired',
  );
});

test('problemRequestBody sends no submission format when none is set', () => {
  assert.equal(problemRequestBody(complete).submission_format, null);
  const files = { cpp: ['main.cpp'] };
  assert.deepEqual(
    problemRequestBody({ ...complete, submissionFormat: files })
      .submission_format,
    files,
  );
});

test('checkerFormatFor starts a new problem on the server default', () => {
  const registered = ['exact', 'lines', 'tokens'];
  assert.equal(checkerFormatFor('', registered, 'lines'), 'lines');
  assert.equal(checkerFormatFor('tokens', registered, 'lines'), 'tokens');
  assert.equal(checkerFormatFor('gone', registered, 'lines'), 'lines');
  assert.equal(checkerFormatFor('', ['exact', 'none'], 'lines'), 'exact');
  assert.equal(checkerFormatFor('', [], 'lines'), '');
});
