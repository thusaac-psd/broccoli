import type { SupportedLanguage } from '../features/problem/api/fetch-supported-languages';

export interface EditorFile {
  filename: string;
  content: string;
}

export interface EditorDraft {
  storageKey: string | undefined;
  language: SupportedLanguage;
  file: EditorFile;
}

type SubmissionFormat = Record<string, string[]> | null | undefined;

function selectedLanguageKey(storageKey: string) {
  return `broccoli-editor-${storageKey}-selected-language`;
}

function getLanguageStorageKeys(storageKey: string, languageId: string) {
  const base = `broccoli-editor-${storageKey}-language-${languageId}`;
  return {
    files: `${base}-files`,
    activeFile: `${base}-active-file`,
    fileContent: (filename: string) =>
      `${base}-file-${encodeURIComponent(filename)}`,
  };
}

function parsePersistedFiles(raw: string | null): EditorFile[] {
  if (!raw) return [];
  try {
    const parsed = JSON.parse(raw) as unknown;
    if (!Array.isArray(parsed)) return [];
    return parsed.filter(
      (item): item is EditorFile =>
        typeof item === 'object' &&
        item !== null &&
        typeof item.filename === 'string' &&
        typeof item.content === 'string',
    );
  } catch {
    return [];
  }
}

export function getConfiguredFilenames(
  languageId: string,
  submissionFormat: SubmissionFormat,
): string[] {
  const names = submissionFormat?.[languageId] ?? [];
  return names
    .map((name) => name.trim())
    .filter(
      (name, index, arr) => name.length > 0 && arr.indexOf(name) === index,
    );
}

export function createEditorFile(
  language: SupportedLanguage,
  submissionFormat: SubmissionFormat,
  file?: EditorFile,
): EditorFile {
  const names = getConfiguredFilenames(language.id, submissionFormat);
  // Legacy ZIP drafts can include directories; submissions use flat filenames.
  const filename = file?.filename.split(/[/\\]/).pop()?.trim();
  return {
    filename:
      names.length === 1
        ? names[0]
        : filename || names[0] || language.defaultFilename,
    content: file?.content ?? language.template,
  };
}

export function restoreEditorDraft(
  storage: Pick<Storage, 'getItem'> | undefined,
  storageKey: string | undefined,
  languages: SupportedLanguage[],
  submissionFormat: SubmissionFormat,
  languageId?: string,
): EditorDraft {
  const savedLanguage =
    languageId ??
    (storageKey ? storage?.getItem(selectedLanguageKey(storageKey)) : null);
  const language =
    languages.find((item) => item.id === savedLanguage) ?? languages[0];
  let file = createEditorFile(language, submissionFormat);

  if (storage && storageKey) {
    const keys = getLanguageStorageKeys(storageKey, language.id);
    // Older editors stored several tabs. Restore only the active one, falling
    // back to the required/default filename, then the most recently added file.
    const savedFiles = parsePersistedFiles(
      storage.getItem(keys.files),
    ).reverse();
    const activeFilename = storage.getItem(keys.activeFile);
    const saved =
      savedFiles.find((item) => item.filename === activeFilename) ??
      savedFiles.find((item) => item.filename === file.filename) ??
      savedFiles[0];
    if (saved) {
      file = createEditorFile(language, submissionFormat, {
        filename: saved.filename,
        content:
          storage.getItem(keys.fileContent(saved.filename)) ?? saved.content,
      });
    }
  }

  return { storageKey, language, file };
}

export function saveEditorDraft(
  storage: Pick<Storage, 'setItem'>,
  draft: EditorDraft,
) {
  if (!draft.storageKey) return;
  const keys = getLanguageStorageKeys(draft.storageKey, draft.language.id);
  storage.setItem(keys.fileContent(draft.file.filename), draft.file.content);
  storage.setItem(
    keys.files,
    JSON.stringify([{ filename: draft.file.filename, content: '' }]),
  );
  storage.setItem(keys.activeFile, draft.file.filename);
  storage.setItem(selectedLanguageKey(draft.storageKey), draft.language.id);
}

export async function readEditorUpload(
  files: readonly File[],
): Promise<
  | { file: EditorFile }
  | { error: 'editor.singleFileOnly' | 'editor.fileReadError' }
> {
  const file = files[0];
  if (
    files.length !== 1 ||
    file.name.toLowerCase().endsWith('.zip') ||
    file.type === 'application/zip' ||
    file.type === 'application/x-zip-compressed'
  ) {
    return { error: 'editor.singleFileOnly' };
  }
  try {
    return { file: { filename: file.name, content: await file.text() } };
  } catch {
    return { error: 'editor.fileReadError' };
  }
}
