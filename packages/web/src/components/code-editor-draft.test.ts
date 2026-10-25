import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { SupportedLanguage } from '../features/problem/api/fetch-supported-languages';
import {
  applyEditorUploads,
  type EditorDraft,
  nextNewFilename,
  normalizeEditorFiles,
  readEditorUploads,
  restoreEditorDraft,
  saveEditorDraft,
} from './code-editor-draft.ts';

const cpp: SupportedLanguage = {
  id: 'cpp',
  name: 'C++',
  extensions: ['cpp', 'cc'],
  defaultFilename: 'solution.cpp',
  template: 'int main() {}',
};
const python: SupportedLanguage = {
  id: 'python3',
  name: 'Python 3',
  extensions: ['py'],
  defaultFilename: 'solution.py',
  template: '',
};
const languages = [cpp, python];
const storageKey = 'u42:problem-9';
const legacyPrefix = `broccoli-editor-${storageKey}-language-cpp`;

function memoryStorage() {
  const values = new Map<string, string>();
  return {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
  };
}

const singleFormat = { cpp: ['solution.cpp'] };
const multiFormat = { cpp: ['main.cpp', 'grader.cpp'] };

function draftOf(
  files: { filename: string; content: string }[],
  activeFilename = files[0].filename,
): EditorDraft {
  return { storageKey, language: cpp, files, activeFilename };
}

test('restores every legacy tab and the active one', () => {
  const storage = memoryStorage();
  storage.setItem(
    `${legacyPrefix}-files`,
    JSON.stringify([
      { filename: 'solution.cpp', content: '' },
      { filename: 'attempt.cpp', content: '' },
    ]),
  );
  storage.setItem(`${legacyPrefix}-active-file`, 'attempt.cpp');
  storage.setItem(`${legacyPrefix}-file-solution.cpp`, 'old solution');
  storage.setItem(`${legacyPrefix}-file-attempt.cpp`, 'current solution');

  const draft = restoreEditorDraft(storage, storageKey, languages, null);
  assert.deepEqual(draft.files, [
    { filename: 'solution.cpp', content: 'old solution' },
    { filename: 'attempt.cpp', content: 'current solution' },
  ]);
  assert.equal(draft.activeFilename, 'attempt.cpp');

  saveEditorDraft(storage, draft);
  assert.deepEqual(
    restoreEditorDraft(storage, storageKey, languages, null),
    draft,
  );
});

test('a single required filename restores the active legacy tab under that name', () => {
  const storage = memoryStorage();
  storage.setItem(
    `${legacyPrefix}-files`,
    JSON.stringify([
      { filename: 'solution.cpp', content: '' },
      { filename: 'attempt.cpp', content: '' },
    ]),
  );
  storage.setItem(`${legacyPrefix}-active-file`, 'attempt.cpp');
  storage.setItem(`${legacyPrefix}-file-solution.cpp`, 'old solution');
  storage.setItem(`${legacyPrefix}-file-attempt.cpp`, 'current solution');

  const draft = restoreEditorDraft(
    storage,
    storageKey,
    languages,
    singleFormat,
  );
  assert.deepEqual(draft.files, [
    { filename: 'solution.cpp', content: 'current solution' },
  ]);
  saveEditorDraft(storage, draft);
  assert.equal(
    storage.getItem(`${legacyPrefix}-file-attempt.cpp`),
    'current solution',
  );
});

test('collapses legacy duplicate filenames to the latest recoverable draft', () => {
  const storage = memoryStorage();
  storage.setItem(
    `${legacyPrefix}-files`,
    JSON.stringify([
      { filename: 'solution.cpp', content: 'old inline draft' },
      { filename: 'solution.cpp', content: 'latest inline draft' },
    ]),
  );
  const draft = restoreEditorDraft(storage, storageKey, languages, null);
  assert.deepEqual(draft.files, [
    { filename: 'solution.cpp', content: 'latest inline draft' },
  ]);
  saveEditorDraft(storage, draft);
  assert.equal(JSON.parse(storage.getItem(`${legacyPrefix}-files`)!).length, 1);
});

test('restores legacy ZIP sources using flat submission filenames', () => {
  const storage = memoryStorage();
  storage.setItem(
    `${legacyPrefix}-files`,
    JSON.stringify([
      { filename: 'old/solution.cpp', content: '' },
      { filename: 'src/solution.cpp', content: '' },
    ]),
  );
  storage.setItem(`${legacyPrefix}-active-file`, 'src/solution.cpp');
  storage.setItem(
    `${legacyPrefix}-file-src%2Fsolution.cpp`,
    'current solution',
  );
  const draft = restoreEditorDraft(storage, storageKey, languages, null);
  assert.deepEqual(draft.files, [
    { filename: 'solution.cpp', content: 'current solution' },
  ]);
  assert.equal(draft.activeFilename, 'solution.cpp');
});

test('without a valid active tab, the first restored tab is active', () => {
  const storage = memoryStorage();
  storage.setItem(
    `${legacyPrefix}-files`,
    JSON.stringify([
      { filename: 'solution.cpp', content: 'solution' },
      { filename: 'helper.h', content: 'header' },
    ]),
  );
  storage.setItem(`${legacyPrefix}-active-file`, 'removed.cpp');
  const draft = restoreEditorDraft(storage, storageKey, languages, null);
  assert.equal(draft.files.length, 2);
  assert.equal(draft.activeFilename, 'solution.cpp');
});

test('several required filenames restore exactly those tabs', () => {
  const storage = memoryStorage();
  storage.setItem(
    `${legacyPrefix}-files`,
    JSON.stringify([
      { filename: 'grader.cpp', content: 'grader' },
      { filename: 'extra.cpp', content: 'extra' },
    ]),
  );
  const draft = restoreEditorDraft(storage, storageKey, languages, multiFormat);
  assert.deepEqual(draft.files, [
    { filename: 'main.cpp', content: cpp.template },
    { filename: 'grader.cpp', content: 'grader' },
  ]);
});

test('drafts survive language changes and remain scoped to their user and problem', () => {
  const storage = memoryStorage();
  const cppDraft = restoreEditorDraft(storage, storageKey, languages, null);
  cppDraft.files[0].content = 'C++ draft';
  saveEditorDraft(storage, cppDraft);
  const pythonDraft = restoreEditorDraft(
    storage,
    storageKey,
    languages,
    null,
    'python3',
  );
  pythonDraft.files[0].content = 'Python draft';
  saveEditorDraft(storage, pythonDraft);

  assert.equal(
    restoreEditorDraft(storage, storageKey, languages, null).language.id,
    'python3',
  );
  assert.equal(
    restoreEditorDraft(storage, storageKey, languages, null, 'cpp').files[0]
      .content,
    'C++ draft',
  );
  for (const otherKey of ['u42:problem-10', 'u43:problem-9']) {
    assert.equal(
      restoreEditorDraft(storage, otherKey, languages, null).files[0].content,
      cpp.template,
    );
  }
});

test('invalid saved metadata falls back to a single language template', () => {
  const storage = memoryStorage();
  for (const raw of ['{', '{}', '[null, 123, {"filename":"bad.cpp"}]']) {
    storage.setItem(`${legacyPrefix}-files`, raw);
    assert.deepEqual(
      restoreEditorDraft(storage, storageKey, languages, null).files,
      [{ filename: 'solution.cpp', content: cpp.template }],
    );
  }
});

test('reads several uploaded files', async () => {
  assert.deepEqual(
    await readEditorUploads([new File(['a'], 'a.cpp'), new File(['b'], 'b.h')]),
    {
      files: [
        { filename: 'a.cpp', content: 'a' },
        { filename: 'b.h', content: 'b' },
      ],
    },
  );
});

test('rejects ZIP uploads', async () => {
  for (const files of [
    [new File(['archive'], 'sources.ZIP')],
    [
      new File(['code'], 'solution.cpp'),
      new File(['archive'], 'sources', { type: 'application/zip' }),
    ],
  ]) {
    assert.deepEqual(await readEditorUploads(files), {
      error: 'editor.zipNotSupported',
    });
  }
});

test('reports file read failures without returning files', async () => {
  const file = new File(['code'], 'solution.cpp');
  file.text = async () => {
    throw new Error('unreadable');
  };
  assert.deepEqual(await readEditorUploads([file]), {
    error: 'editor.fileReadError',
  });
});

test('a single required filename renames an imported file', () => {
  const draft = draftOf([{ filename: 'solution.cpp', content: 'old' }]);
  const result = applyEditorUploads(
    draft,
    [{ filename: 'main.cpp', content: 'answer' }],
    singleFormat,
  );
  assert.deepEqual(result.draft.files, [
    { filename: 'solution.cpp', content: 'answer' },
  ]);
  assert.deepEqual(result.loadedInto, [
    { from: 'main.cpp', to: 'solution.cpp' },
  ]);
});

test('free uploads overwrite a same-name tab instead of duplicating it', () => {
  const draft = draftOf([
    { filename: 'solution.cpp', content: 'edited' },
    { filename: 'helper.h', content: 'header' },
  ]);
  const result = applyEditorUploads(
    draft,
    [{ filename: 'solution.cpp', content: 'uploaded' }],
    null,
  );
  assert.deepEqual(result.draft.files, [
    { filename: 'solution.cpp', content: 'uploaded' },
    { filename: 'helper.h', content: 'header' },
  ]);
});

test('free uploads with new names open tabs and replace an untouched template', () => {
  const untouched = draftOf([
    { filename: 'solution.cpp', content: cpp.template },
  ]);
  const result = applyEditorUploads(
    untouched,
    [
      { filename: 'a.cpp', content: 'a' },
      { filename: 'b.h', content: 'b' },
    ],
    null,
  );
  assert.deepEqual(result.draft.files, [
    { filename: 'a.cpp', content: 'a' },
    { filename: 'b.h', content: 'b' },
  ]);
  assert.equal(result.draft.activeFilename, 'a.cpp');
});

test('required tabs are filled by name and never added', () => {
  const draft = draftOf([
    { filename: 'main.cpp', content: '' },
    { filename: 'grader.cpp', content: '' },
  ]);
  const result = applyEditorUploads(
    draft,
    [
      { filename: 'grader.cpp', content: 'g' },
      { filename: 'extra.cpp', content: 'x' },
    ],
    multiFormat,
  );
  assert.deepEqual(result.draft.files, [
    { filename: 'main.cpp', content: '' },
    { filename: 'grader.cpp', content: 'g' },
  ]);
  assert.deepEqual(result.skipped, ['extra.cpp']);
  assert.equal(result.draft.activeFilename, 'grader.cpp');
});

test('a lone odd-named upload fills the active required tab', () => {
  const draft = draftOf(
    [
      { filename: 'main.cpp', content: '' },
      { filename: 'grader.cpp', content: '' },
    ],
    'grader.cpp',
  );
  const result = applyEditorUploads(
    draft,
    [{ filename: 'mine.cpp', content: 'm' }],
    multiFormat,
  );
  assert.equal(result.draft.files[1].content, 'm');
  assert.deepEqual(result.loadedInto, [{ from: 'mine.cpp', to: 'grader.cpp' }]);
});

test('nothing changes when no upload matches a required tab', () => {
  const draft = draftOf([
    { filename: 'main.cpp', content: '' },
    { filename: 'grader.cpp', content: '' },
  ]);
  const uploads = [
    { filename: 'x.cpp', content: 'x' },
    { filename: 'y.cpp', content: 'y' },
  ];
  const result = applyEditorUploads(draft, uploads, multiFormat);
  assert.equal(result.draft, draft);
  assert.deepEqual(result.skipped, ['x.cpp', 'y.cpp']);
});

test('new tabs get an unused name', () => {
  const draft = draftOf([
    { filename: 'solution.cpp', content: '' },
    { filename: 'file1.cpp', content: '' },
  ]);
  assert.equal(nextNewFilename(draft), 'file2.cpp');
});

test('a late single required filename keeps real code over an empty active tab', () => {
  // The editor can mount before the problem (and its format) has loaded.
  assert.deepEqual(
    normalizeEditorFiles(
      cpp,
      singleFormat,
      [
        { filename: 'solution.cpp', content: 'real code' },
        { filename: 'file1.cpp', content: '' },
      ],
      'file1.cpp',
    ),
    [{ filename: 'solution.cpp', content: 'real code' }],
  );
});

test('a single required filename prefers the active tab, then the matching name', () => {
  const files = [
    { filename: 'attempt.cpp', content: 'attempt' },
    { filename: 'solution.cpp', content: 'solution' },
    { filename: 'scratch.cpp', content: 'scratch' },
  ];
  assert.equal(
    normalizeEditorFiles(cpp, singleFormat, files, 'attempt.cpp')[0].content,
    'attempt',
  );
  assert.equal(
    normalizeEditorFiles(cpp, singleFormat, files, null)[0].content,
    'solution',
  );
});

test('several unmatched uploads change nothing for a single required filename', () => {
  const draft = draftOf([{ filename: 'solution.cpp', content: 'kept' }]);
  const result = applyEditorUploads(
    draft,
    [
      { filename: 'x.cpp', content: 'x' },
      { filename: 'y.cpp', content: 'y' },
    ],
    singleFormat,
  );
  assert.equal(result.draft, draft);
  assert.deepEqual(result.skipped, ['x.cpp', 'y.cpp']);
});
