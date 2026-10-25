import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { SupportedLanguage } from '../features/problem/api/fetch-supported-languages';
import {
  createEditorFile,
  readEditorUpload,
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

test('restores the previously active legacy tab and leaves other saved contents intact', () => {
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
  assert.deepEqual(draft.file, {
    filename: 'attempt.cpp',
    content: 'current solution',
  });

  saveEditorDraft(storage, draft);
  assert.deepEqual(JSON.parse(storage.getItem(`${legacyPrefix}-files`)!), [
    { filename: 'attempt.cpp', content: '' },
  ]);
  assert.equal(
    storage.getItem(`${legacyPrefix}-file-solution.cpp`),
    'old solution',
  );
  assert.deepEqual(
    restoreEditorDraft(storage, storageKey, languages, null).file,
    draft.file,
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
  assert.equal(draft.file.content, 'latest inline draft');
  saveEditorDraft(storage, draft);
  assert.equal(JSON.parse(storage.getItem(`${legacyPrefix}-files`)!).length, 1);
});

test('restores the active legacy ZIP source using a flat submission filename', () => {
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
  assert.deepEqual(
    restoreEditorDraft(storage, storageKey, languages, null).file,
    { filename: 'solution.cpp', content: 'current solution' },
  );
});

test('without a valid active tab, restores the default file before another legacy file', () => {
  const storage = memoryStorage();
  storage.setItem(
    `${legacyPrefix}-files`,
    JSON.stringify([
      { filename: 'solution.cpp', content: 'solution' },
      { filename: 'helper.h', content: 'header' },
    ]),
  );
  storage.setItem(`${legacyPrefix}-active-file`, 'removed.cpp');
  assert.deepEqual(
    restoreEditorDraft(storage, storageKey, languages, null).file,
    { filename: 'solution.cpp', content: 'solution' },
  );
});

test('drafts survive language changes and remain scoped to their user and problem', () => {
  const storage = memoryStorage();
  const cppDraft = restoreEditorDraft(storage, storageKey, languages, null);
  cppDraft.file.content = 'C++ draft';
  saveEditorDraft(storage, cppDraft);
  const pythonDraft = restoreEditorDraft(
    storage,
    storageKey,
    languages,
    null,
    'python3',
  );
  pythonDraft.file.content = 'Python draft';
  saveEditorDraft(storage, pythonDraft);

  assert.equal(
    restoreEditorDraft(storage, storageKey, languages, null).language.id,
    'python3',
  );
  assert.equal(
    restoreEditorDraft(storage, storageKey, languages, null, 'cpp').file
      .content,
    'C++ draft',
  );
  for (const otherKey of ['u42:problem-10', 'u43:problem-9']) {
    assert.equal(
      restoreEditorDraft(storage, otherKey, languages, null).file.content,
      cpp.template,
    );
  }
});

test('uses a single required filename for imported code', async () => {
  const result = await readEditorUpload([new File(['answer'], 'main.cpp')]);
  assert.ok('file' in result);
  assert.deepEqual(
    createEditorFile(cpp, { cpp: ['solution.cpp'] }, result.file),
    {
      filename: 'solution.cpp',
      content: 'answer',
    },
  );
});

test('repeated uploads replace the persisted source instead of accumulating files', async () => {
  const storage = memoryStorage();
  for (const content of ['first upload', 'second upload']) {
    const result = await readEditorUpload([
      new File([content], 'solution.cpp'),
    ]);
    assert.ok('file' in result);
    saveEditorDraft(storage, { storageKey, language: cpp, file: result.file });
  }
  assert.deepEqual(JSON.parse(storage.getItem(`${legacyPrefix}-files`)!), [
    { filename: 'solution.cpp', content: '' },
  ]);
  assert.equal(
    restoreEditorDraft(storage, storageKey, languages, null).file.content,
    'second upload',
  );
});

test('rejects multiple dropped files and ZIP uploads', async () => {
  const source = new File(['code'], 'solution.cpp');
  for (const files of [
    [],
    [source, source],
    [new File(['archive'], 'sources.ZIP')],
    [new File(['archive'], 'sources', { type: 'application/zip' })],
  ]) {
    assert.deepEqual(await readEditorUpload(files), {
      error: 'editor.singleFileOnly',
    });
  }
});

test('reports file read failures without returning a replacement draft', async () => {
  const file = new File(['code'], 'solution.cpp');
  file.text = async () => {
    throw new Error('unreadable');
  };
  assert.deepEqual(await readEditorUpload([file]), {
    error: 'editor.fileReadError',
  });
});

test('invalid saved metadata falls back to a single language template', () => {
  const storage = memoryStorage();
  for (const raw of ['{', '{}', '[null, 123, {"filename":"bad.cpp"}]']) {
    storage.setItem(`${legacyPrefix}-files`, raw);
    assert.deepEqual(
      restoreEditorDraft(storage, storageKey, languages, null).file,
      {
        filename: 'solution.cpp',
        content: cpp.template,
      },
    );
  }
});
