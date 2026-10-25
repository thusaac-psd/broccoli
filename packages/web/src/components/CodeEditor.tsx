import '@/lib/monaco-setup';

import { useApiClient } from '@broccoli/web-sdk/api';
import { useAuth } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Slot } from '@broccoli/web-sdk/slot';
import { useSubmitGating } from '@broccoli/web-sdk/submission';
import { useTheme } from '@broccoli/web-sdk/theme';
import {
  Button,
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@broccoli/web-sdk/ui';
import type { Monaco } from '@monaco-editor/react';
import Editor from '@monaco-editor/react';
import { useQuery } from '@tanstack/react-query';
import JSZip from 'jszip';
import {
  ChevronDown,
  ChevronRight,
  Maximize2,
  Minimize2,
  Play,
  Plus,
  Terminal,
  Upload,
  X,
} from 'lucide-react';
import type { editor } from 'monaco-editor';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';

import { KeybindingModeDropdown } from '@/components/KeybindingModeDropdown';
import {
  fetchSupportedLanguages,
  type SupportedLanguage,
} from '@/features/problem/api/fetch-supported-languages';
import type { SubmissionEntry } from '@/features/submission/hooks/use-submissions';
import { useEditorKeybindings } from '@/lib/use-editor-keybindings';

type Language = SupportedLanguage;

const FALLBACK_LANGUAGE: Language = {
  id: 'plaintext',
  name: 'Plain Text',
  defaultFilename: 'solution.txt',
  template: '',
  extensions: [],
};

export interface EditorFile {
  id: string;
  filename: string;
  content: string;
}

interface CodeEditorProps {
  onSubmit?: (files: EditorFile[], language: string) => void;
  onRun?: (
    files: EditorFile[],
    language: string,
    customTestCases: { input: string; expected_output?: string | null }[],
  ) => void;
  latestRun?: SubmissionEntry | null;
  isFullscreen?: boolean;
  onToggleFullscreen?: () => void;
  /** Unique key for persisting code to localStorage (e.g. problem ID). */
  storageKey?: string;
  /** Currently selected contest type. */
  contestType?: string;
  /** Callback when contest type changes. */
  onContestTypeChange?: (contestType: string) => void;
  /** Available contest types from registry. */
  contestTypes?: string[];
  /**
   * Server-provided file names per language (from problem.submission_format).
   * Keys are language ids (e.g. "cpp", "java"), values are arrays of filenames.
   * Takes precedence over the built-in FILENAME_MAP defaults.
   */
  submissionFormat?: Record<string, string[]> | null;
}

const EXT_TO_MONACO: Record<string, string> = {
  cpp: 'cpp',
  cc: 'cpp',
  cxx: 'cpp',
  'c++': 'cpp',
  hpp: 'cpp',
  h: 'c',
  c: 'c',
  py: 'python',
  java: 'java',
  js: 'javascript',
  ts: 'typescript',
  json: 'json',
  xml: 'xml',
  txt: 'plaintext',
  md: 'markdown',
  sh: 'shell',
  yml: 'yaml',
  yaml: 'yaml',
};

function getMonacoLanguage(filename: string): string {
  const ext = filename.split('.').pop()?.toLowerCase() ?? '';
  return EXT_TO_MONACO[ext] ?? 'plaintext';
}

function detectLanguageFromFiles(
  files: EditorFile[],
  dynamicExtMap: Record<string, string>,
): string | null {
  for (const file of files) {
    const ext = file.filename.split('.').pop()?.toLowerCase() ?? '';
    const langId = dynamicExtMap[ext];
    if (langId) return langId;
  }
  return null;
}

function getStorageKeys(storageKey: string) {
  return {
    selectedLanguage: `broccoli-editor-${storageKey}-selected-language`,
  };
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

type PersistedEditorFile = {
  filename: string;
  content: string;
};

function parsePersistedFiles(raw: string | null): PersistedEditorFile[] {
  if (!raw) return [];
  try {
    const parsed = JSON.parse(raw) as unknown;
    if (!Array.isArray(parsed)) return [];
    return parsed
      .filter(
        (item): item is PersistedEditorFile =>
          typeof item === 'object' &&
          item !== null &&
          typeof (item as PersistedEditorFile).filename === 'string' &&
          typeof (item as PersistedEditorFile).content === 'string',
      )
      .map((item) => ({
        filename: item.filename,
        content: item.content,
      }));
  } catch {
    return [];
  }
}

function loadLanguageFiles(
  storageKey: string,
  languageId: string,
): PersistedEditorFile[] {
  const keys = getLanguageStorageKeys(storageKey, languageId);
  const filenames = parsePersistedFiles(localStorage.getItem(keys.files)).map(
    (file) => file.filename,
  );

  return filenames.map((filename) => ({
    filename,
    content: localStorage.getItem(keys.fileContent(filename)) ?? '',
  }));
}

function saveLanguageFiles(
  storageKey: string,
  languageId: string,
  files: EditorFile[],
) {
  const keys = getLanguageStorageKeys(storageKey, languageId);
  const persisted = files.map((file) => ({
    filename: file.filename,
    content: '',
  }));
  localStorage.setItem(keys.files, JSON.stringify(persisted));
  for (const file of files) {
    localStorage.setItem(keys.fileContent(file.filename), file.content);
  }
}

let fileIdCounter = 0;
function nextFileId() {
  return `file-${++fileIdCounter}-${Date.now()}`;
}

const FILENAME_MAP: Record<string, string> = {
  cpp: 'solution.cpp',
  c: 'solution.c',
  python3: 'solution.py',
  java: 'Main.java',
  javascript: 'solution.js',
  typescript: 'solution.ts',
  rust: 'solution.rs',
  go: 'solution.go',
};

/**
 * Returns the default filename for a language, preferring server-provided
 * submission_format over the built-in FILENAME_MAP.
 */
function getDefaultFilename(
  languageId: string,
  submissionFormat?: Record<string, string[]> | null,
): string {
  if (submissionFormat) {
    const serverFiles = submissionFormat[languageId];
    if (serverFiles && serverFiles.length > 0) {
      return serverFiles[0];
    }
  }
  return FILENAME_MAP[languageId] ?? 'solution.txt';
}

function getConfiguredFilenames(
  languageId: string,
  submissionFormat?: Record<string, string[]> | null,
): string[] {
  const names = submissionFormat?.[languageId] ?? [];
  return names
    .map((name) => name.trim())
    .filter(
      (name, index, arr) => name.length > 0 && arr.indexOf(name) === index,
    );
}

export function CodeEditor(props: CodeEditorProps) {
  const apiClient = useApiClient();
  const { data: supportedLanguages, isPending } = useQuery({
    queryKey: ['supported-languages'],
    queryFn: () => fetchSupportedLanguages(apiClient),
    staleTime: 5 * 60 * 1000,
  });

  // Editor state (saved language + draft files) must never initialize against
  // the fallback language list: the saved language id would not be found, the
  // draft for the real language would not load, and the auto-save effect would
  // immediately overwrite the contestant's persisted draft with the template.
  // Gate mounting the stateful editor on the supported-languages query so the
  // initializers always see the real language list.
  if (isPending) {
    return (
      <div className="h-full flex flex-col p-3">
        <div className="flex-1 min-h-0 rounded-lg border bg-muted/30 animate-pulse" />
      </div>
    );
  }

  return (
    <CodeEditorContent
      {...props}
      supportedLanguages={supportedLanguages ?? []}
    />
  );
}

function CodeEditorContent({
  onSubmit,
  onRun,
  latestRun,
  isFullscreen,
  onToggleFullscreen,
  storageKey: storageKeyProp,
  contestType,
  onContestTypeChange,
  contestTypes,
  submissionFormat,
  supportedLanguages,
}: CodeEditorProps & { supportedLanguages: Language[] }) {
  const { t } = useTranslation();
  const { user, isLoading: authLoading } = useAuth();
  // Scope the localStorage draft keys by the signed-in user so a shared contest
  // machine never surfaces one contestant's in-progress solution to the next.
  //
  // The id is captured in a sticky ref (only ever updated to a non-null value)
  // so a transient refresh blip (user -> null -> user) does NOT flip the key and
  // trip the reset effect that would wipe in-progress code. And we persist only
  // once auth has RESOLVED and a user is known: during the pre-auth window (and
  // for logged-out users) the key is undefined, so nothing is written to an
  // unscoped key that would be orphaned once the id resolves.
  const userIdRef = useRef<number | null>(null);
  if (user?.id != null) {
    userIdRef.current = user.id;
  }
  const storageKey =
    !authLoading && storageKeyProp && userIdRef.current != null
      ? `u${userIdRef.current}:${storageKeyProp}`
      : undefined;
  const gating = useSubmitGating();
  const isGated = gating?.isBlocked ?? false;

  const availableLanguages = useMemo(() => {
    if (supportedLanguages.length === 0) return [FALLBACK_LANGUAGE];
    if (!submissionFormat) return supportedLanguages;
    const configured = supportedLanguages.filter((lang) => {
      const files = getConfiguredFilenames(lang.id, submissionFormat);
      return files.length > 0;
    });
    return configured.length > 0 ? configured : supportedLanguages;
  }, [submissionFormat, supportedLanguages]);

  const dynamicExtMap = useMemo(() => {
    const map: Record<string, string> = {};
    for (const lang of availableLanguages) {
      for (const ext of lang.extensions) {
        if (!map[ext]) map[ext] = lang.id;
      }
    }
    return map;
  }, [availableLanguages]);

  const isSubmissionFormatLocked = useMemo(
    () =>
      Object.keys(submissionFormat ?? {}).some(
        (languageId) =>
          getConfiguredFilenames(languageId, submissionFormat).length > 0,
      ),
    [submissionFormat],
  );

  const [selectedLanguage, setSelectedLanguage] = useState<Language>(() => {
    if (storageKey) {
      const saved = localStorage.getItem(
        getStorageKeys(storageKey).selectedLanguage,
      );
      if (saved) {
        const found = availableLanguages.find((l) => l.id === saved);
        if (found) return found;
      }
    }
    return availableLanguages[0];
  });

  const buildFilesForLanguage = useCallback(
    (language: Language, previousFiles: EditorFile[]): EditorFile[] => {
      const configuredNames = getConfiguredFilenames(
        language.id,
        submissionFormat,
      );
      if (configuredNames.length === 0) {
        const fallbackFilename = getDefaultFilename(
          language.id,
          submissionFormat,
        );
        const keep = previousFiles.find((f) => f.filename === fallbackFilename);
        return [
          {
            id: keep?.id ?? nextFileId(),
            filename: fallbackFilename,
            content: keep?.content ?? language.template,
          },
        ];
      }

      return configuredNames.map((filename, index) => {
        const keep = previousFiles.find((f) => f.filename === filename);
        return {
          id: keep?.id ?? nextFileId(),
          filename,
          content: keep?.content ?? (index === 0 ? language.template : ''),
        };
      });
    },
    [submissionFormat],
  );

  // Reconcile when the available language list changes (e.g. the problem's
  // submission format loads and filters out the current selection). Perform
  // the full restore path: prefer the persisted language choice and its saved
  // draft files over resetting to the first language's template.
  useEffect(() => {
    if (availableLanguages.some((lang) => lang.id === selectedLanguage.id))
      return;
    let nextLanguage = availableLanguages[0];
    if (storageKey) {
      const savedLang = localStorage.getItem(
        getStorageKeys(storageKey).selectedLanguage,
      );
      const found = savedLang
        ? availableLanguages.find((l) => l.id === savedLang)
        : undefined;
      if (found) nextLanguage = found;
    }
    setSelectedLanguage(nextLanguage);
    const persisted = storageKey
      ? loadLanguageFiles(storageKey, nextLanguage.id).map((file) => ({
          id: nextFileId(),
          filename: file.filename,
          content: file.content,
        }))
      : [];
    setFiles((prev) => {
      const source = persisted.length > 0 ? persisted : prev;
      const nextFiles = buildFilesForLanguage(nextLanguage, source);
      if (nextFiles.length > 0) {
        const savedActiveFilename = storageKey
          ? localStorage.getItem(
              getLanguageStorageKeys(storageKey, nextLanguage.id).activeFile,
            )
          : null;
        const active = savedActiveFilename
          ? nextFiles.find((file) => file.filename === savedActiveFilename)
          : undefined;
        setActiveFileId(active?.id ?? nextFiles[0].id);
      }
      return nextFiles;
    });
  }, [
    availableLanguages,
    buildFilesForLanguage,
    selectedLanguage.id,
    storageKey,
  ]);

  // Multi-file tabs state
  const [files, setFiles] = useState<EditorFile[]>(() => {
    const savedFiles = storageKey
      ? loadLanguageFiles(storageKey, selectedLanguage.id).map((file) => ({
          id: nextFileId(),
          filename: file.filename,
          content: file.content,
        }))
      : [];

    const initial = buildFilesForLanguage(selectedLanguage, savedFiles);
    return initial;
  });

  const [activeFileId, setActiveFileId] = useState<string>(() => {
    if (!storageKey || files.length === 0) return files[0]?.id ?? nextFileId();
    const savedActiveFilename = localStorage.getItem(
      getLanguageStorageKeys(storageKey, selectedLanguage.id).activeFile,
    );
    if (!savedActiveFilename) return files[0].id;
    const found = files.find((file) => file.filename === savedActiveFilename);
    return found?.id ?? files[0].id;
  });

  const activeFile = files.find((f) => f.id === activeFileId) ?? files[0];

  const editorRef = useRef<editor.IStandaloneCodeEditor | null>(null);
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const vimStatusRef = useRef<HTMLDivElement | null>(null);
  const [editorInstance, setEditorInstance] =
    useState<editor.IStandaloneCodeEditor | null>(null);
  const [monacoInstance, setMonacoInstance] = useState<Monaco>(null);
  const [keybindingMode, setKeybindingMode] = useEditorKeybindings(
    editorInstance,
    monacoInstance,
    vimStatusRef,
  );
  const { theme } = useTheme();

  // Reset editor state when storageKey changes (switching problems)
  const prevStorageKey = useRef(storageKey);
  useEffect(() => {
    if (storageKey === prevStorageKey.current) return;
    prevStorageKey.current = storageKey;

    // Load saved language
    let lang = availableLanguages[0];
    if (storageKey) {
      const savedLang = localStorage.getItem(
        getStorageKeys(storageKey).selectedLanguage,
      );
      if (savedLang) {
        const found = availableLanguages.find((l) => l.id === savedLang);
        if (found) lang = found;
      }
    }
    setSelectedLanguage(lang);

    const savedFiles = storageKey
      ? loadLanguageFiles(storageKey, lang.id).map((file) => ({
          id: nextFileId(),
          filename: file.filename,
          content: file.content,
        }))
      : [];

    const withSaved = buildFilesForLanguage(lang, savedFiles);
    setFiles(withSaved);
    if (withSaved.length > 0) {
      const savedActiveFilename = storageKey
        ? localStorage.getItem(
            getLanguageStorageKeys(storageKey, lang.id).activeFile,
          )
        : null;
      const active = savedActiveFilename
        ? withSaved.find((file) => file.filename === savedActiveFilename)
        : undefined;
      setActiveFileId(active?.id ?? withSaved[0].id);
    }
  }, [availableLanguages, buildFilesForLanguage, storageKey]);

  // Auto-save all files and current language to localStorage
  useEffect(() => {
    if (!storageKey) return;
    const keys = getStorageKeys(storageKey);

    saveLanguageFiles(storageKey, selectedLanguage.id, files);

    const active = files.find((file) => file.id === activeFileId) ?? files[0];
    if (active) {
      localStorage.setItem(
        getLanguageStorageKeys(storageKey, selectedLanguage.id).activeFile,
        active.filename,
      );
    }

    localStorage.setItem(keys.selectedLanguage, selectedLanguage.id);
  }, [storageKey, files, selectedLanguage, activeFileId]);

  const handleLanguageChange = useCallback(
    (language: Language) => {
      setSelectedLanguage(language);
      const persisted = storageKey
        ? loadLanguageFiles(storageKey, language.id).map((file) => ({
            id: nextFileId(),
            filename: file.filename,
            content: file.content,
          }))
        : [];
      setFiles((prev) => {
        const source = persisted.length > 0 ? persisted : prev;
        const next = buildFilesForLanguage(language, source);
        if (next.length > 0) {
          const savedActiveFilename = storageKey
            ? localStorage.getItem(
                getLanguageStorageKeys(storageKey, language.id).activeFile,
              )
            : null;
          const active = savedActiveFilename
            ? next.find((file) => file.filename === savedActiveFilename)
            : undefined;
          setActiveFileId(active?.id ?? next[0].id);
        }
        return next;
      });
    },
    [buildFilesForLanguage, storageKey],
  );

  const updateFileContent = useCallback((fileId: string, content: string) => {
    setFiles((prev) =>
      prev.map((f) => (f.id === fileId ? { ...f, content } : f)),
    );
  }, []);

  const closeFile = useCallback(
    (fileId: string) => {
      if (isSubmissionFormatLocked) return;
      setFiles((prev) => {
        if (prev.length <= 1) return prev; // Don't close last file
        const next = prev.filter((f) => f.id !== fileId);
        if (activeFileId === fileId) {
          setActiveFileId(next[0].id);
        }
        return next;
      });
    },
    [activeFileId, isSubmissionFormatLocked],
  );

  const addFiles = useCallback(
    (newFiles: EditorFile[]) => {
      if (newFiles.length === 0) return;

      // Detect language from uploaded files
      const detectedLang = detectLanguageFromFiles(newFiles, dynamicExtMap);
      if (detectedLang) {
        const lang = availableLanguages.find((l) => l.id === detectedLang);
        if (lang) setSelectedLanguage(lang);
      }

      setFiles((prev) => {
        // If there's only the default template file, replace it
        const currentDefault = getDefaultFilename(
          selectedLanguage.id,
          submissionFormat,
        );
        const isDefault =
          prev.length === 1 &&
          prev[0].content === selectedLanguage.template &&
          prev[0].filename === currentDefault;

        const base = isDefault ? [] : prev;
        return [...base, ...newFiles];
      });

      // Activate the first new file
      setActiveFileId(newFiles[0].id);
    },
    [availableLanguages, dynamicExtMap, selectedLanguage, submissionFormat],
  );

  const processUploadedFiles = useCallback(
    async (fileList: FileList) => {
      const newFiles: EditorFile[] = [];

      for (const file of Array.from(fileList)) {
        if (
          file.name.endsWith('.zip') ||
          file.type === 'application/zip' ||
          file.type === 'application/x-zip-compressed'
        ) {
          // Process zip file
          try {
            const arrayBuffer = await file.arrayBuffer();
            const zip = await JSZip.loadAsync(arrayBuffer);

            const entries: { path: string; file: JSZip.JSZipObject }[] = [];
            zip.forEach((relativePath, zipEntry) => {
              if (!zipEntry.dir) {
                entries.push({ path: relativePath, file: zipEntry });
              }
            });

            for (const entry of entries) {
              // Skip hidden files / OS metadata
              const name = entry.path.split('/').pop() ?? entry.path;
              if (name.startsWith('.') || name.startsWith('__MACOSX')) continue;
              if (entry.path.includes('__MACOSX/')) continue;

              try {
                const content = await entry.file.async('string');
                newFiles.push({
                  id: nextFileId(),
                  filename: entry.path,
                  content,
                });
              } catch {
                // Skip binary files that can't be read as text
              }
            }
          } catch (err) {
            console.error('Failed to process zip file:', err);
          }
        } else {
          // Process regular file
          try {
            const content = await file.text();
            newFiles.push({
              id: nextFileId(),
              filename: file.name,
              content,
            });
          } catch (err) {
            console.error('Failed to read file:', err);
          }
        }
      }

      addFiles(newFiles);
    },
    [addFiles],
  );

  const addNewFile = useCallback(() => {
    if (isSubmissionFormatLocked) return;
    const ext =
      getDefaultFilename(selectedLanguage.id, submissionFormat)
        .split('.')
        .pop() ?? 'txt';
    // Find a unique name like "file1.cpp", "file2.cpp", etc.
    let index = 1;
    const existingNames = new Set(files.map((f) => f.filename));
    while (existingNames.has(`file${index}.${ext}`)) index++;
    const filename = `file${index}.${ext}`;
    const newFile: EditorFile = { id: nextFileId(), filename, content: '' };
    setFiles((prev) => [...prev, newFile]);
    setActiveFileId(newFile.id);
  }, [isSubmissionFormatLocked, selectedLanguage, submissionFormat, files]);

  const handleUploadClick = () => {
    fileInputRef.current?.click();
  };

  const handleFileInputChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    if (e.target.files && e.target.files.length > 0) {
      processUploadedFiles(e.target.files);
      // Reset so the same file can be selected again
      e.target.value = '';
    }
  };

  const handleSubmit = () => {
    if (onSubmit) {
      onSubmit(files, selectedLanguage.id);
    }
  };

  const [customTestCases, setCustomTestCases] = useState([
    { input: '', expectedOutput: '' },
  ]);
  const [activeTestCase, setActiveTestCase] = useState(0);
  const [showCustomInput, setShowCustomInput] = useState(false);

  const updateTestCase = useCallback(
    (index: number, field: 'input' | 'expectedOutput', value: string) => {
      setCustomTestCases((prev) =>
        prev.map((tc, i) => (i === index ? { ...tc, [field]: value } : tc)),
      );
    },
    [],
  );

  const addTestCase = useCallback(() => {
    setCustomTestCases((prev) => {
      if (prev.length >= 10) return prev;
      const next = [...prev, { input: '', expectedOutput: '' }];
      setActiveTestCase(next.length - 1);
      return next;
    });
  }, []);

  const removeTestCase = useCallback(
    (index: number) => {
      setCustomTestCases((prev) => {
        if (prev.length <= 1) return prev;
        const next = prev.filter((_, i) => i !== index);
        if (activeTestCase >= next.length) {
          setActiveTestCase(next.length - 1);
        }
        return next;
      });
    },
    [activeTestCase],
  );

  const handleRun = () => {
    if (!onRun) return;
    if (!showCustomInput) {
      setShowCustomInput(true);
      return;
    }
    const tcs = customTestCases.map((tc) => ({
      input: tc.input,
      // Stored answer files end with a single trailing newline, and exact
      // checkers compare byte-for-byte. Normalize the user's expected output
      // the same way (strip trailing whitespace, append one '\n') so a custom
      // run agrees with the real judge instead of reporting WrongAnswer for
      // the trailing newline that every correct program prints.
      expected_output:
        tc.expectedOutput.trim().length === 0
          ? null
          : `${tc.expectedOutput.trimEnd()}\n`,
    }));
    onRun(files, selectedLanguage.id, tcs);
  };

  const handleEditorDidMount = (
    ed: editor.IStandaloneCodeEditor,
    monaco: Monaco,
  ) => {
    editorRef.current = ed;
    setEditorInstance(ed);
    setMonacoInstance(monaco);
  };

  const handleDrop = useCallback(
    (e: React.DragEvent) => {
      // Only intercept file drops, let dockview panel drags pass through
      if (!e.dataTransfer.types.includes('Files')) return;
      e.preventDefault();
      e.stopPropagation();
      if (e.dataTransfer.files.length > 0) {
        processUploadedFiles(e.dataTransfer.files);
      }
    },
    [processUploadedFiles],
  );

  const handleDragOver = useCallback((e: React.DragEvent) => {
    if (!e.dataTransfer.types.includes('Files')) return;
    e.preventDefault();
    e.stopPropagation();
  }, []);

  const toolbar = (
    <div className="flex items-center gap-1">
      <Slot
        name="problem-detail.editor.toolbar"
        as="div"
        className="flex items-center gap-1"
      />
      {contestTypes && contestTypes.length > 0 && onContestTypeChange && (
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button
              variant="ghost"
              size="sm"
              className="h-7 px-2 text-xs gap-1 text-muted-foreground"
            >
              {contestType || t('admin.field.selectPlaceholder')}
              <ChevronDown className="h-3 w-3" />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start">
            {contestTypes.map((ct) => (
              <DropdownMenuItem
                key={ct}
                onClick={() => onContestTypeChange(ct)}
              >
                {ct}
              </DropdownMenuItem>
            ))}
          </DropdownMenuContent>
        </DropdownMenu>
      )}
      <Button
        variant="ghost"
        size="sm"
        className="h-7 w-7 p-0 text-muted-foreground"
        onClick={handleUploadClick}
        title={t('editor.upload')}
      >
        <Upload className="h-3.5 w-3.5" />
      </Button>
      <input
        ref={fileInputRef}
        type="file"
        multiple
        accept=".c,.cpp,.cc,.cxx,.h,.hpp,.py,.java,.js,.ts,.txt,.json,.xml,.md,.sh,.yml,.yaml,.zip"
        className="hidden"
        onChange={handleFileInputChange}
      />
      {onToggleFullscreen && (
        <Button
          variant="ghost"
          size="sm"
          className="h-7 w-7 p-0 text-muted-foreground"
          onClick={onToggleFullscreen}
          aria-label={t('editor.toggleFullscreen')}
        >
          {isFullscreen ? (
            <Minimize2 className="h-3.5 w-3.5" />
          ) : (
            <Maximize2 className="h-3.5 w-3.5" />
          )}
        </Button>
      )}
      <div className="h-4 w-px bg-border mx-1" />
      <KeybindingModeDropdown
        mode={keybindingMode}
        onChange={setKeybindingMode}
      />
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button
            variant="ghost"
            size="sm"
            className="h-7 px-2 text-xs gap-1 font-medium"
          >
            {selectedLanguage.name}
            <ChevronDown className="h-3 w-3" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          {availableLanguages.map((lang) => (
            <DropdownMenuItem
              key={lang.id}
              onClick={() => handleLanguageChange(lang)}
            >
              {lang.name}
            </DropdownMenuItem>
          ))}
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );

  const fileTabs = (
    <div className="flex items-center gap-0 overflow-x-auto border-b">
      {files.map((file) => (
        <button
          key={file.id}
          type="button"
          onClick={() => setActiveFileId(file.id)}
          className={`group flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium border-b-2 transition-colors whitespace-nowrap ${
            file.id === activeFileId
              ? 'border-primary text-foreground'
              : 'border-transparent text-muted-foreground hover:text-foreground hover:border-muted-foreground/30'
          }`}
        >
          <span>{file.filename.split('/').pop()}</span>
          {files.length > 1 && !isSubmissionFormatLocked && (
            <span
              role="button"
              tabIndex={0}
              onClick={(e) => {
                e.stopPropagation();
                closeFile(file.id);
              }}
              onKeyDown={(e) => {
                if (e.key === 'Enter' || e.key === ' ') {
                  e.stopPropagation();
                  closeFile(file.id);
                }
              }}
              className="opacity-0 group-hover:opacity-100 hover:text-destructive transition-opacity"
            >
              <X className="h-3 w-3" />
            </span>
          )}
        </button>
      ))}
      {!isSubmissionFormatLocked && (
        <button
          type="button"
          onClick={addNewFile}
          title={t('editor.addFile')}
          className="flex items-center px-2 py-1.5 text-muted-foreground hover:text-foreground transition-colors border-b-2 border-transparent"
        >
          <Plus className="h-3.5 w-3.5" />
        </button>
      )}
    </div>
  );

  const editorArea = (
    <div className="flex-1 min-h-0 border rounded-lg overflow-hidden flex flex-col">
      <div className="flex-1 min-h-0">
        <Editor
          height="100%"
          language={getMonacoLanguage(activeFile.filename)}
          value={activeFile.content}
          onChange={(value) => updateFileContent(activeFile.id, value || '')}
          onMount={handleEditorDidMount}
          theme={theme === 'dark' ? 'vs-dark' : 'vs'}
          options={{
            minimap: { enabled: false },
            fontSize: 14,
            lineNumbers: 'on',
            roundedSelection: false,
            scrollBeyondLastLine: false,
            automaticLayout: true,
            tabSize: 4,
            wordWrap: 'on',
          }}
        />
      </div>
      <div
        ref={vimStatusRef}
        className={`vim-status-bar h-6 shrink-0 border-t border-border bg-muted/40 px-3 font-mono text-[11px] leading-6 text-muted-foreground ${keybindingMode !== 'vim' ? 'hidden' : ''}`}
      />
    </div>
  );

  // Auto-expand custom input panel when a run completes
  useEffect(() => {
    if (latestRun && latestRun.status !== 'submitting') {
      setShowCustomInput(true);
    }
  }, [latestRun?.status]);

  const runResult = latestRun?.codeRun?.result;
  const runTcResults = runResult?.test_case_results ?? [];
  const activeTc = customTestCases[activeTestCase];
  const activeRunTc = runTcResults.find((r) => r.run_index === activeTestCase);
  const activeCustomTc =
    latestRun?.codeRun?.custom_test_cases?.[activeTestCase];
  const activeHasExpected = activeCustomTc?.expected_output != null;

  const customInputPanel = (
    <div className="flex flex-col gap-1.5">
      <button
        type="button"
        onClick={() => setShowCustomInput((v) => !v)}
        className="flex items-center gap-1.5 px-1 py-1 text-xs font-medium text-muted-foreground hover:text-foreground transition-colors self-start"
      >
        {showCustomInput ? (
          <ChevronDown className="h-3 w-3" />
        ) : (
          <ChevronRight className="h-3 w-3" />
        )}
        <Terminal className="h-3 w-3" />
        {t('editor.customInput')}
      </button>
      {showCustomInput && (
        <div className="flex flex-col gap-2">
          {/* Test case tabs */}
          <div
            className="flex items-center gap-0 text-xs overflow-x-auto"
            style={{ scrollbarWidth: 'none' }}
          >
            {customTestCases.map((_, i) => (
              <button
                key={i}
                type="button"
                onClick={() => setActiveTestCase(i)}
                className={`group flex items-center gap-1 px-2.5 py-1 rounded-t-md border-b-2 whitespace-nowrap transition-colors ${
                  i === activeTestCase
                    ? 'border-primary text-foreground font-medium'
                    : 'border-transparent text-muted-foreground hover:text-foreground'
                }`}
              >
                Case {i + 1}
                {customTestCases.length > 1 && (
                  <span
                    role="button"
                    tabIndex={0}
                    onClick={(e) => {
                      e.stopPropagation();
                      removeTestCase(i);
                    }}
                    onKeyDown={(e) => {
                      if (e.key === 'Enter' || e.key === ' ') {
                        e.stopPropagation();
                        removeTestCase(i);
                      }
                    }}
                    className="opacity-0 group-hover:opacity-100 hover:text-destructive ml-0.5"
                  >
                    <X className="h-3 w-3" />
                  </span>
                )}
              </button>
            ))}
            {customTestCases.length < 10 && (
              <button
                type="button"
                onClick={addTestCase}
                className="px-1.5 py-1 text-muted-foreground hover:text-foreground transition-colors"
              >
                <Plus className="h-3.5 w-3.5" />
              </button>
            )}
          </div>

          {/* Active test case editor */}
          {activeTc && (
            <div className="grid grid-cols-2 gap-2">
              <div className="flex flex-col gap-1">
                <label className="text-xs font-medium text-muted-foreground">
                  stdin
                </label>
                <textarea
                  value={activeTc.input}
                  onChange={(e) =>
                    updateTestCase(activeTestCase, 'input', e.target.value)
                  }
                  placeholder={t('editor.inputPlaceholder')}
                  spellCheck={false}
                  className="w-full resize-y rounded-md border bg-muted/50 px-3 py-2 font-mono text-sm leading-relaxed placeholder:text-muted-foreground/50 focus:outline-none focus:ring-1 focus:ring-ring min-h-[4.5rem] max-h-[12rem]"
                />
              </div>
              <div className="flex flex-col gap-1">
                <label className="text-xs font-medium text-muted-foreground">
                  {t('editor.expectedOutput')}
                  <span className="ml-1 text-muted-foreground/60 font-normal">
                    ({t('editor.optional')})
                  </span>
                </label>
                <textarea
                  value={activeTc.expectedOutput}
                  onChange={(e) =>
                    updateTestCase(
                      activeTestCase,
                      'expectedOutput',
                      e.target.value,
                    )
                  }
                  placeholder={t('editor.expectedOutputPlaceholder')}
                  spellCheck={false}
                  className="w-full resize-y rounded-md border bg-muted/50 px-3 py-2 font-mono text-sm leading-relaxed placeholder:text-muted-foreground/50 focus:outline-none focus:ring-1 focus:ring-ring min-h-[4.5rem] max-h-[12rem]"
                />
              </div>
            </div>
          )}

          {/* Run status / results */}
          {latestRun &&
            (latestRun.status === 'submitting' ||
              (latestRun.status === 'polling' && !runResult)) && (
              <div className="rounded-md border bg-muted/30 px-3 py-2 text-sm text-muted-foreground animate-pulse">
                {t('editor.running')}
              </div>
            )}
          {latestRun && latestRun.status === 'error' && latestRun.error && (
            <div className="rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-destructive">
              {latestRun.error.message}
            </div>
          )}
          {runResult && latestRun?.status === 'done' && activeRunTc && (
            <div className="rounded-md border bg-muted/30 overflow-hidden">
              <div className="flex items-center gap-3 px-3 py-1.5 border-b bg-muted/50 text-xs">
                {latestRun.codeRun?.status === 'CompilationError' ? (
                  <span className="font-medium text-amber-600 dark:text-amber-400">
                    {t('editor.compilationError')}
                  </span>
                ) : activeHasExpected ? (
                  <span
                    className={
                      activeRunTc.verdict === 'Accepted'
                        ? 'font-medium text-green-600 dark:text-green-400'
                        : 'font-medium text-red-600 dark:text-red-400'
                    }
                  >
                    {activeRunTc.verdict}
                  </span>
                ) : (
                  <span className="font-medium text-foreground">
                    {t('editor.executed')}
                  </span>
                )}
                {activeRunTc.time_used != null && (
                  <span className="text-muted-foreground">
                    {activeRunTc.time_used} ms
                  </span>
                )}
                {activeRunTc.memory_used != null && (
                  <span className="text-muted-foreground">
                    {(activeRunTc.memory_used / 1024).toFixed(1)} MB
                  </span>
                )}
              </div>
              {runResult.compile_output && (
                <div className="px-3 py-2 border-b">
                  <div className="text-xs font-medium text-muted-foreground mb-1">
                    {t('editor.compilerOutput')}
                  </div>
                  <pre className="font-mono text-xs whitespace-pre-wrap text-foreground/80 max-h-[8rem] overflow-y-auto">
                    {runResult.compile_output}
                  </pre>
                </div>
              )}
              {activeRunTc.stdout != null && (
                <div className="px-3 py-2">
                  <div className="text-xs font-medium text-muted-foreground mb-1">
                    stdout
                  </div>
                  <pre className="font-mono text-sm whitespace-pre-wrap text-foreground max-h-[10rem] overflow-y-auto">
                    {activeRunTc.stdout || t('editor.emptyOutput')}
                  </pre>
                </div>
              )}
              {activeRunTc.stderr && (
                <div className="px-3 py-2 border-t">
                  <div className="text-xs font-medium text-muted-foreground mb-1">
                    stderr
                  </div>
                  <pre className="font-mono text-xs whitespace-pre-wrap text-foreground/70 max-h-[6rem] overflow-y-auto">
                    {activeRunTc.stderr}
                  </pre>
                </div>
              )}
              {!runResult.compile_output &&
                !activeRunTc.stdout &&
                !activeRunTc.stderr && (
                  <div className="px-3 py-2 text-sm text-muted-foreground">
                    {t('editor.noOutput')}
                  </div>
                )}
            </div>
          )}
          {/* Compilation error shown once (not per test case) */}
          {runResult &&
            latestRun?.status === 'done' &&
            latestRun.codeRun?.status === 'CompilationError' &&
            !activeRunTc && (
              <div className="rounded-md border bg-muted/30 overflow-hidden">
                <div className="flex items-center gap-3 px-3 py-1.5 border-b bg-muted/50 text-xs">
                  <span className="font-medium text-amber-600 dark:text-amber-400">
                    {t('editor.compilationError')}
                  </span>
                </div>
                {runResult.compile_output && (
                  <div className="px-3 py-2">
                    <pre className="font-mono text-xs whitespace-pre-wrap text-foreground/80 max-h-[8rem] overflow-y-auto">
                      {runResult.compile_output}
                    </pre>
                  </div>
                )}
              </div>
            )}
        </div>
      )}
    </div>
  );

  const actionButtons = (
    <div className="flex flex-col items-end gap-1.5">
      <div className="flex gap-2">
        <Button variant="outline" onClick={handleRun}>
          <Play className="mr-2 h-4 w-4" />
          {t('editor.run')}
        </Button>
        <Button onClick={handleSubmit} disabled={isGated}>
          {t('editor.submit')}
        </Button>
      </div>
      {isGated && gating?.blockReason && (
        <span className="text-xs text-muted-foreground">
          {gating.blockReason}
        </span>
      )}
    </div>
  );

  return (
    <div
      className="h-full flex flex-col"
      onDrop={handleDrop}
      onDragOver={handleDragOver}
    >
      <div className="flex items-center justify-between px-2 py-1.5 shrink-0 border-b">
        {toolbar}
      </div>
      {fileTabs}
      <div className="flex-1 flex flex-col gap-3 p-3 min-h-0">
        {editorArea}
        {customInputPanel}
        {actionButtons}
      </div>
    </div>
  );
}
