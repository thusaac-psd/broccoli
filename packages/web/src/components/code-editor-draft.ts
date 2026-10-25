import type { SupportedLanguage } from '../features/problem/api/fetch-supported-languages';

export interface EditorFile {
  filename: string;
  content: string;
}

/**
 * Editor state for one problem and language. Tabs are keyed by filename, so
 * `files` never holds two files with the same name (the server rejects
 * duplicate filenames).
 */
export interface EditorDraft {
  storageKey: string | undefined;
  language: SupportedLanguage;
  files: EditorFile[];
  activeFilename: string;
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

// Legacy ZIP drafts can include directories; submissions use flat filenames.
function flatFilename(filename: string): string {
  return filename.split(/[/\\]/).pop()?.trim() ?? '';
}

/**
 * Shapes `files` to what the problem accepts for `language`. Later files
 * win over earlier ones with the same name:
 * - one required filename: a single tab under that name. When several tabs
 *   compete (an older multi-tab draft, or the problem's format loading after
 *   the editor), it keeps the first non-blank of the active tab, the tab
 *   already named like the requirement, then the newest tab, so a
 *   contestant's code is never swapped for an empty tab;
 * - several required filenames: exactly those tabs, filled by name;
 * - no required filenames: the files as given, flattened and de-duplicated,
 *   or the language's default file.
 */
export function normalizeEditorFiles(
  language: SupportedLanguage,
  submissionFormat: SubmissionFormat,
  files: EditorFile[] = [],
  activeFilename?: string | null,
): EditorFile[] {
  const flat = files
    .map((file) => ({ ...file, filename: flatFilename(file.filename) }))
    .filter((file) => file.filename.length > 0);
  const names = getConfiguredFilenames(language.id, submissionFormat);

  if (names.length === 1) {
    const newestFirst = [...flat].reverse();
    const named = (name: string) =>
      newestFirst.find((file) => file.filename === name);
    const candidates = [
      activeFilename ? named(flatFilename(activeFilename)) : undefined,
      named(names[0]),
      ...newestFirst,
    ].filter((file): file is EditorFile => file !== undefined);
    const chosen =
      candidates.find((file) => file.content.trim().length > 0) ??
      candidates[0];
    return [
      { filename: names[0], content: chosen?.content ?? language.template },
    ];
  }
  if (names.length > 1) {
    return names.map((filename, index) => ({
      filename,
      content:
        [...flat].reverse().find((file) => file.filename === filename)
          ?.content ?? (index === 0 ? language.template : ''),
    }));
  }

  const byName = new Map<string, EditorFile>();
  for (const file of flat) byName.set(file.filename, file);
  if (byName.size === 0) {
    return [{ filename: language.defaultFilename, content: language.template }];
  }
  return [...byName.values()];
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

  let saved: EditorFile[] = [];
  let activeFilename: string | null = null;
  if (storage && storageKey) {
    const keys = getLanguageStorageKeys(storageKey, language.id);
    saved = parsePersistedFiles(storage.getItem(keys.files)).map((item) => ({
      filename: item.filename,
      content: storage.getItem(keys.fileContent(item.filename)) ?? item.content,
    }));
    activeFilename = storage.getItem(keys.activeFile);
  }

  const files = normalizeEditorFiles(
    language,
    submissionFormat,
    saved,
    activeFilename,
  );
  const flatActive = activeFilename ? flatFilename(activeFilename) : null;
  return {
    storageKey,
    language,
    files,
    activeFilename:
      files.find((file) => file.filename === flatActive)?.filename ??
      files[0].filename,
  };
}

export function saveEditorDraft(
  storage: Pick<Storage, 'setItem'>,
  draft: EditorDraft,
) {
  if (!draft.storageKey) return;
  const keys = getLanguageStorageKeys(draft.storageKey, draft.language.id);
  for (const file of draft.files) {
    storage.setItem(keys.fileContent(file.filename), file.content);
  }
  storage.setItem(
    keys.files,
    JSON.stringify(
      draft.files.map((file) => ({ filename: file.filename, content: '' })),
    ),
  );
  storage.setItem(keys.activeFile, draft.activeFilename);
  storage.setItem(selectedLanguageKey(draft.storageKey), draft.language.id);
}

export async function readEditorUploads(
  files: readonly File[],
): Promise<
  | { files: EditorFile[] }
  | { error: 'editor.zipNotSupported' | 'editor.fileReadError' }
> {
  if (
    files.some(
      (file) =>
        file.name.toLowerCase().endsWith('.zip') ||
        file.type === 'application/zip' ||
        file.type === 'application/x-zip-compressed',
    )
  ) {
    return { error: 'editor.zipNotSupported' };
  }
  try {
    return {
      files: await Promise.all(
        files.map(async (file) => ({
          filename: file.name,
          content: await file.text(),
        })),
      ),
    };
  } catch {
    return { error: 'editor.fileReadError' };
  }
}

export interface UploadResult {
  draft: EditorDraft;
  /** Uploads placed under a different name than their own. */
  loadedInto: { from: string; to: string }[];
  /** Uploads that matched no required filename and were not placed. */
  skipped: string[];
}

/**
 * Puts uploaded files into `draft`. With required filenames, uploads only
 * fill those tabs: a file named like a tab fills it, a lone file with any
 * other name fills the active tab, and the rest are skipped. Without
 * required filenames, an upload overwrites the tab with its name or opens a
 * new one, and replaces an untouched default file. When nothing can be
 * placed, the draft comes back unchanged.
 */
export function applyEditorUploads(
  draft: EditorDraft,
  uploads: EditorFile[],
  submissionFormat: SubmissionFormat,
): UploadResult {
  const result: UploadResult = { draft, loadedInto: [], skipped: [] };
  const flat = uploads.map((file) => ({
    ...file,
    filename: flatFilename(file.filename),
  }));
  if (flat.length === 0) return result;
  const names = getConfiguredFilenames(draft.language.id, submissionFormat);

  if (names.length > 0) {
    const contents = new Map<string, string>();
    for (const upload of flat) {
      if (names.includes(upload.filename)) {
        contents.set(upload.filename, upload.content);
      } else if (flat.length === 1) {
        const target = names.includes(draft.activeFilename)
          ? draft.activeFilename
          : names[0];
        contents.set(target, upload.content);
        result.loadedInto.push({ from: upload.filename, to: target });
      } else {
        result.skipped.push(upload.filename);
      }
    }
    if (contents.size === 0) return result;
    result.draft = {
      ...draft,
      files: draft.files.map((file) =>
        contents.has(file.filename)
          ? { ...file, content: contents.get(file.filename)! }
          : file,
      ),
      activeFilename: [...contents.keys()][0],
    };
    return result;
  }

  const [only] = draft.files;
  const untouchedDefault =
    draft.files.length === 1 &&
    only.filename === draft.language.defaultFilename &&
    only.content === draft.language.template;
  result.draft = {
    ...draft,
    files: normalizeEditorFiles(draft.language, submissionFormat, [
      ...(untouchedDefault ? [] : draft.files),
      ...flat,
    ]),
    activeFilename: flat[0].filename,
  };
  return result;
}

/** A filename like `file1.cpp` that no open tab uses yet. */
export function nextNewFilename(draft: EditorDraft): string {
  const ext = draft.language.defaultFilename.split('.').pop() ?? 'txt';
  const taken = new Set(draft.files.map((file) => file.filename));
  let index = 1;
  while (taken.has(`file${index}.${ext}`)) index++;
  return `file${index}.${ext}`;
}
