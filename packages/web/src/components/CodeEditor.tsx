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
import { toast } from 'sonner';

import { KeybindingModeDropdown } from '@/components/KeybindingModeDropdown';
import {
  fetchSupportedLanguages,
  type SupportedLanguage,
} from '@/features/problem/api/fetch-supported-languages';
import type { SubmissionEntry } from '@/features/submission/hooks/use-submissions';
import { useEditorKeybindings } from '@/lib/use-editor-keybindings';

import {
  createEditorFile,
  type EditorFile,
  getConfiguredFilenames,
  readEditorUpload,
  restoreEditorDraft,
  saveEditorDraft,
} from './code-editor-draft';

export type { EditorFile } from './code-editor-draft';

type Language = SupportedLanguage;

const FALLBACK_LANGUAGE: Language = {
  id: 'plaintext',
  name: 'Plain Text',
  defaultFilename: 'solution.txt',
  template: '',
  extensions: [],
};

interface CodeEditorProps {
  onSubmit?: (file: EditorFile, language: string) => void;
  onRun?: (
    file: EditorFile,
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
   * Takes precedence over the language's default filename.
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

export function CodeEditor(props: CodeEditorProps) {
  const apiClient = useApiClient();
  const { data: supportedLanguages, isPending } = useQuery({
    queryKey: ['supported-languages'],
    queryFn: () => fetchSupportedLanguages(apiClient),
    staleTime: 5 * 60 * 1000,
  });

  // Editor state (saved language + draft file) must never initialize against
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

  const [draft, setDraft] = useState(() =>
    restoreEditorDraft(
      storageKey ? localStorage : undefined,
      storageKey,
      availableLanguages,
      submissionFormat,
    ),
  );
  const selectedLanguage = draft.language;
  const activeFile = useMemo(
    () => createEditorFile(selectedLanguage, submissionFormat, draft.file),
    [selectedLanguage, submissionFormat, draft.file],
  );
  const requiresMultipleFiles =
    getConfiguredFilenames(selectedLanguage.id, submissionFormat).length > 1;

  // Restore the single draft when switching problems/users or when the
  // problem's submission format makes the selected language unavailable.
  useEffect(() => {
    if (
      draft.storageKey === storageKey &&
      availableLanguages.some((lang) => lang.id === selectedLanguage.id)
    ) {
      return;
    }
    setDraft(
      restoreEditorDraft(
        storageKey ? localStorage : undefined,
        storageKey,
        availableLanguages,
        submissionFormat,
      ),
    );
  }, [
    availableLanguages,
    draft.storageKey,
    selectedLanguage.id,
    storageKey,
    submissionFormat,
  ]);

  const editorRef = useRef<editor.IStandaloneCodeEditor | null>(null);
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const uploadState = useRef({ generation: 0 });
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

  // Ignore a pending file read after changing problems/languages or unmounting.
  useEffect(() => {
    const pendingUpload = uploadState.current;
    return () => {
      pendingUpload.generation++;
    };
  }, [storageKey, selectedLanguage.id]);

  // Do not save the previous problem's file under a newly selected problem's
  // key while the restore effect above is still updating the editor state.
  useEffect(() => {
    if (
      !storageKey ||
      draft.storageKey !== storageKey ||
      !availableLanguages.some((lang) => lang.id === selectedLanguage.id)
    ) {
      return;
    }
    saveEditorDraft(localStorage, { ...draft, file: activeFile });
  }, [storageKey, draft, activeFile, availableLanguages, selectedLanguage.id]);

  const handleLanguageChange = useCallback(
    (language: Language) => {
      setDraft(
        restoreEditorDraft(
          storageKey ? localStorage : undefined,
          storageKey,
          availableLanguages,
          submissionFormat,
          language.id,
        ),
      );
    },
    [availableLanguages, storageKey, submissionFormat],
  );

  const updateFileContent = useCallback((content: string) => {
    setDraft((prev) => ({ ...prev, file: { ...prev.file, content } }));
  }, []);

  const processUploadedFiles = useCallback(
    async (fileList: FileList) => {
      const generation = ++uploadState.current.generation;
      const result = await readEditorUpload(Array.from(fileList));
      if (generation !== uploadState.current.generation) return;
      if ('error' in result) {
        toast.error(t(result.error));
        return;
      }

      const ext = result.file.filename.split('.').pop()?.toLowerCase() ?? '';
      const language =
        availableLanguages.find((lang) => lang.extensions.includes(ext)) ??
        selectedLanguage;
      setDraft({
        storageKey,
        language,
        file: createEditorFile(language, submissionFormat, result.file),
      });
    },
    [availableLanguages, selectedLanguage, storageKey, submissionFormat, t],
  );

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
    if (onSubmit && !requiresMultipleFiles) {
      onSubmit(activeFile, selectedLanguage.id);
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
    onRun(activeFile, selectedLanguage.id, tcs);
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
        accept=".c,.cpp,.cc,.cxx,.h,.hpp,.py,.java,.js,.ts,.txt,.json,.xml,.md,.sh,.yml,.yaml"
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

  const editorArea = (
    <div className="flex-1 min-h-0 border rounded-lg overflow-hidden flex flex-col">
      <div className="flex-1 min-h-0">
        <Editor
          height="100%"
          language={getMonacoLanguage(activeFile.filename)}
          value={activeFile.content}
          onChange={(value) => updateFileContent(value || '')}
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
        <Button
          onClick={handleSubmit}
          disabled={isGated || requiresMultipleFiles}
        >
          {t('editor.submit')}
        </Button>
      </div>
      {requiresMultipleFiles && (
        <span className="text-xs text-muted-foreground">
          {t('editor.multipleFilesRequired')}
        </span>
      )}
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
      <div className="border-b px-3 py-1.5 text-xs font-medium truncate">
        {activeFile.filename}
      </div>
      <div className="flex-1 flex flex-col gap-3 p-3 min-h-0">
        {editorArea}
        {customInputPanel}
        {actionButtons}
      </div>
    </div>
  );
}
