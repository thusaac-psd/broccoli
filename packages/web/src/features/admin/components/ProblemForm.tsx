import { useApiClient } from '@broccoli/web-sdk/api';
import { useRegistries } from '@broccoli/web-sdk/hooks';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import {
  Button,
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
  Input,
  Label,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Separator,
} from '@broccoli/web-sdk/ui';
import { useQuery } from '@tanstack/react-query';
import { ChevronDown, Plus, X } from 'lucide-react';
import { useEffect, useMemo, useState } from 'react';

import { MarkdownEditor } from '@/components/MarkdownEditor';
import { MarkdownEditorWithAttachments } from '@/features/admin/components/MarkdownEditorWithAttachments';
import { SwitchField } from '@/features/admin/components/SwitchField';
import type { Attachment } from '@/features/problem/api/attachments';
import {
  fetchSupportedLanguages,
  type SupportedLanguage,
} from '@/features/problem/api/fetch-supported-languages';

export interface ProblemFormData {
  title: string;
  content: string;
  timeLimit: number;
  memoryLimit: number;
  problemType: string;
  checkerFormat: string;
  defaultContestType: string;
  showTestDetails: boolean;
  isPublic: boolean;
  submissionFormat: Record<string, string[]>;
}

interface ProblemFormProps {
  data: ProblemFormData;
  onChange: (data: ProblemFormData) => void;
  problemId?: number;
  attachments?: Attachment[];
}

function fallbackDefaultFilename(languageId: string): string {
  const filenameMap: Record<string, string> = {
    cpp: 'solution.cpp',
    c: 'solution.c',
    python3: 'solution.py',
    java: 'Main.java',
    javascript: 'solution.js',
    typescript: 'solution.ts',
    rust: 'solution.rs',
    go: 'solution.go',
  };
  return filenameMap[languageId] ?? 'solution.txt';
}

function getDefaultFilename(language: SupportedLanguage): string {
  return language.defaultFilename || fallbackDefaultFilename(language.id);
}

export function ProblemForm({
  data,
  onChange,
  problemId,
  attachments,
}: ProblemFormProps) {
  const { t } = useTranslation();
  const apiClient = useApiClient();
  const { data: registries } = useRegistries();
  const { data: supportedLanguages = [] } = useQuery({
    queryKey: ['supported-languages'],
    queryFn: () => fetchSupportedLanguages(apiClient),
    staleTime: 5 * 60 * 1000,
  });
  const [selectedLanguage, setSelectedLanguage] = useState('');
  const [newFilenameByLanguage, setNewFilenameByLanguage] = useState<
    Record<string, string>
  >({});

  const problemTypes = useMemo(
    () => registries?.problem_types ?? [],
    [registries],
  );
  const checkerFormats = useMemo(
    () => registries?.checker_formats ?? [],
    [registries],
  );
  const contestTypes = useMemo(
    () => registries?.contest_types ?? [],
    [registries],
  );

  // Effect to auto-select the first available option if the current value is invalid/empty
  useEffect(() => {
    let changed = false;
    const newData = { ...data };

    if (!problemTypes.includes(data.problemType)) {
      newData.problemType = problemTypes[0] ?? '';
      changed = true;
    }
    if (!checkerFormats.includes(data.checkerFormat)) {
      newData.checkerFormat = checkerFormats[0] ?? '';
      changed = true;
    }
    // Deliberately no auto-pick for the contest type: it decides who judges
    // practice submissions, so the admin must choose it. An unknown value is
    // cleared back to the placeholder instead of silently replaced.
    if (
      data.defaultContestType &&
      !contestTypes.includes(data.defaultContestType)
    ) {
      newData.defaultContestType = '';
      changed = true;
    }

    if (changed) {
      onChange(newData);
    }
  }, [data, problemTypes, checkerFormats, contestTypes, onChange]);

  const handleTitleChange = (title: string) => {
    onChange({ ...data, title });
  };

  const handleContentChange = (content: string) => {
    onChange({ ...data, content });
  };

  const handleTimeLimitChange = (timeLimit: number) => {
    onChange({ ...data, timeLimit });
  };

  const handleMemoryLimitChange = (memoryLimit: number) => {
    onChange({ ...data, memoryLimit });
  };

  const handleProblemTypeChange = (problemType: string) => {
    onChange({ ...data, problemType });
  };

  const handleCheckerFormatChange = (checkerFormat: string) => {
    onChange({ ...data, checkerFormat });
  };

  const handleDefaultContestTypeChange = (defaultContestType: string) => {
    onChange({ ...data, defaultContestType });
  };

  const handleShowTestDetailsChange = (showTestDetails: boolean) => {
    onChange({ ...data, showTestDetails });
  };

  const handleIsPublicChange = (isPublic: boolean) => {
    onChange({ ...data, isPublic });
  };

  const configuredLanguages = useMemo(
    () => Object.keys(data.submissionFormat),
    [data.submissionFormat],
  );

  const languageById = useMemo(
    () => new Map(supportedLanguages.map((lang) => [lang.id, lang])),
    [supportedLanguages],
  );

  const canAddLanguages = useMemo(
    () =>
      supportedLanguages.filter(
        (lang) => !configuredLanguages.includes(lang.id),
      ),
    [supportedLanguages, configuredLanguages],
  );

  const selectedLanguageLabel = useMemo(() => {
    if (!selectedLanguage) return t('admin.submissionFormat.language');
    const language = languageById.get(selectedLanguage);
    if (!language) return selectedLanguage;
    return `${language.name} (${language.id})`;
  }, [languageById, selectedLanguage, t]);

  const addLanguage = () => {
    const lang = selectedLanguage.trim();
    if (!lang || data.submissionFormat[lang]) return;
    const languageMeta = languageById.get(lang);
    const defaultFilename = languageMeta
      ? getDefaultFilename(languageMeta)
      : fallbackDefaultFilename(lang);
    onChange({
      ...data,
      submissionFormat: {
        ...data.submissionFormat,
        [lang]: [defaultFilename],
      },
    });
    setSelectedLanguage('');
  };

  const removeLanguage = (lang: string) => {
    const next = { ...data.submissionFormat };
    delete next[lang];
    onChange({ ...data, submissionFormat: next });
  };

  const addFilename = (lang: string) => {
    const filename = (newFilenameByLanguage[lang] ?? '').trim();
    if (!filename) return;
    const existing = data.submissionFormat[lang] ?? [];
    if (existing.includes(filename)) return;
    onChange({
      ...data,
      submissionFormat: {
        ...data.submissionFormat,
        [lang]: [...existing, filename],
      },
    });
    setNewFilenameByLanguage((prev) => ({ ...prev, [lang]: '' }));
  };

  const removeFilename = (lang: string, filename: string) => {
    const existing = data.submissionFormat[lang] ?? [];
    onChange({
      ...data,
      submissionFormat: {
        ...data.submissionFormat,
        [lang]: existing.filter((name) => name !== filename),
      },
    });
  };

  return (
    <>
      <div className="space-y-2">
        <Label htmlFor="problem-title">{t('admin.field.title')}</Label>
        <Input
          id="problem-title"
          value={data.title}
          onChange={(e) => handleTitleChange(e.target.value)}
          required
          maxLength={256}
          placeholder="Two Sum"
        />
      </div>

      <div className="space-y-2">
        <Label htmlFor="problem-content">{t('admin.field.content')}</Label>
        {problemId != null && attachments ? (
          <>
            <MarkdownEditorWithAttachments
              id="problem-content"
              value={data.content}
              onChange={handleContentChange}
              minHeight={250}
              placeholder="Problem statement (Markdown supported)"
              problemId={problemId}
              attachments={attachments}
            />
            <p className="text-xs text-muted-foreground">
              {t('admin.attachments.editorHint')}
            </p>
          </>
        ) : (
          <MarkdownEditor
            id="problem-content"
            value={data.content}
            onChange={handleContentChange}
            minHeight={250}
            placeholder="Problem statement (Markdown supported)"
          />
        )}
      </div>

      <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
        <div className="space-y-2">
          <Label htmlFor="problem-time">{t('admin.field.timeLimit')}</Label>
          <Input
            id="problem-time"
            type="number"
            min={1}
            max={30000}
            value={data.timeLimit}
            onChange={(e) => handleTimeLimitChange(Number(e.target.value))}
            required
          />
        </div>
        <div className="space-y-2">
          <Label htmlFor="problem-memory">{t('admin.field.memoryLimit')}</Label>
          <Input
            id="problem-memory"
            type="number"
            min={1}
            max={1048576}
            value={data.memoryLimit}
            onChange={(e) => handleMemoryLimitChange(Number(e.target.value))}
            required
          />
        </div>
      </div>

      <div className="grid grid-cols-1 gap-4 sm:grid-cols-3">
        <div className="space-y-2">
          <Label htmlFor="problem-type">{t('admin.field.problemType')}</Label>
          <Select
            value={data.problemType}
            onValueChange={handleProblemTypeChange}
            disabled={problemTypes.length === 0}
          >
            <SelectTrigger id="problem-type">
              <SelectValue placeholder={t('admin.field.selectPlaceholder')} />
            </SelectTrigger>
            <SelectContent>
              {problemTypes.map((opt) => (
                <SelectItem key={opt} value={opt}>
                  {opt}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>

        <div className="space-y-2">
          <Label htmlFor="checker-format">
            {t('admin.field.checkerFormat')}
          </Label>
          <Select
            value={data.checkerFormat}
            onValueChange={handleCheckerFormatChange}
            disabled={checkerFormats.length === 0}
          >
            <SelectTrigger id="checker-format">
              <SelectValue placeholder={t('admin.field.selectPlaceholder')} />
            </SelectTrigger>
            <SelectContent>
              {checkerFormats.map((opt) => (
                <SelectItem key={opt} value={opt}>
                  {opt}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>

        <div className="space-y-2">
          <Label htmlFor="default-contest-type">
            {t('admin.field.contestType')}
          </Label>
          <Select
            value={data.defaultContestType}
            onValueChange={handleDefaultContestTypeChange}
            disabled={contestTypes.length === 0}
          >
            <SelectTrigger id="default-contest-type">
              <SelectValue placeholder={t('admin.field.selectPlaceholder')} />
            </SelectTrigger>
            <SelectContent>
              {contestTypes.map((opt) => (
                <SelectItem key={opt} value={opt}>
                  {opt}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
      </div>

      <Separator />

      <div className="space-y-3">
        <Label className="text-sm text-muted-foreground">
          {t('admin.field.options')}
        </Label>
        <SwitchField
          id="problem-test-details"
          label={t('admin.field.showTestDetails')}
          checked={data.showTestDetails}
          onCheckedChange={handleShowTestDetailsChange}
        />
        <SwitchField
          id="problem-is-public"
          label={t('admin.field.problemIsPublic')}
          description={t('admin.field.problemIsPublicHelp')}
          checked={data.isPublic}
          onCheckedChange={handleIsPublicChange}
        />
      </div>

      <Separator />

      <div className="space-y-3">
        <Label className="text-sm text-muted-foreground">
          {t('admin.field.submissionFormat')}
        </Label>

        <div className="flex gap-2">
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button
                type="button"
                variant="outline"
                className="flex-1 justify-between"
                disabled={canAddLanguages.length === 0}
              >
                <span className="truncate">{selectedLanguageLabel}</span>
                <ChevronDown className="ml-2 h-4 w-4" />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="start">
              {canAddLanguages.map((lang) => (
                <DropdownMenuItem
                  key={lang.id}
                  onClick={() => setSelectedLanguage(lang.id)}
                >
                  {lang.name} ({lang.id})
                </DropdownMenuItem>
              ))}
            </DropdownMenuContent>
          </DropdownMenu>
          <Button
            type="button"
            variant="outline"
            onClick={addLanguage}
            disabled={!selectedLanguage || canAddLanguages.length === 0}
          >
            <Plus className="h-4 w-4 mr-1" />
            {t('admin.submissionFormat.addLanguage')}
          </Button>
        </div>

        {configuredLanguages.length === 0 ? (
          <div className="text-sm text-muted-foreground">
            {t('admin.submissionFormat.empty')}
          </div>
        ) : (
          <div className="space-y-3">
            {configuredLanguages.map((lang) => (
              <div key={lang} className="rounded-md border p-3 space-y-2">
                <div className="flex items-center justify-between">
                  <div className="text-sm font-medium">{lang}</div>
                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    className="h-7 px-2 text-destructive hover:text-destructive"
                    onClick={() => removeLanguage(lang)}
                  >
                    <X className="h-3.5 w-3.5 mr-1" />
                    {t('admin.delete')}
                  </Button>
                </div>

                <div className="flex flex-wrap gap-2">
                  {(data.submissionFormat[lang] ?? []).map((filename) => (
                    <span
                      key={`${lang}-${filename}`}
                      className="inline-flex items-center gap-1 rounded-md border px-2 py-1 text-xs"
                    >
                      <span>{filename}</span>
                      <button
                        type="button"
                        onClick={() => removeFilename(lang, filename)}
                        className="text-muted-foreground hover:text-destructive"
                      >
                        <X className="h-3 w-3" />
                      </button>
                    </span>
                  ))}
                </div>

                <div className="flex gap-2">
                  <Input
                    value={newFilenameByLanguage[lang] ?? ''}
                    placeholder={t(
                      'admin.submissionFormat.filenamePlaceholder',
                    )}
                    onChange={(e) =>
                      setNewFilenameByLanguage((prev) => ({
                        ...prev,
                        [lang]: e.target.value,
                      }))
                    }
                  />
                  <Button
                    type="button"
                    variant="outline"
                    onClick={() => addFilename(lang)}
                  >
                    <Plus className="h-4 w-4 mr-1" />
                    {t('admin.submissionFormat.addFile')}
                  </Button>
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
    </>
  );
}
