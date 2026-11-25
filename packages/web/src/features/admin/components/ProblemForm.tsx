import {
  loadedData,
  registryChoiceLabel,
  registryChoices,
  useQueryState,
  useRegistries,
} from '@broccoli/web-sdk/hooks';
import { useIdentifierLabel, useTranslation } from '@broccoli/web-sdk/i18n';
import {
  Button,
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
  Input,
  Label,
  QueryErrorState,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Separator,
} from '@broccoli/web-sdk/ui';
import { ChevronDown, Plus, X } from 'lucide-react';
import { useEffect, useMemo, useState } from 'react';

import { MarkdownEditor } from '@/components/MarkdownEditor';
import { MarkdownEditorWithAttachments } from '@/features/admin/components/MarkdownEditorWithAttachments';
import { SwitchField } from '@/features/admin/components/SwitchField';
import {
  checkerFormatFor,
  type ProblemFormData,
} from '@/features/admin/problem-form-values';
import type { Attachment } from '@/features/problem/api/attachments';
import { useSupportedLanguages } from '@/features/problem/hooks/use-supported-languages';
import type { SupportedLanguage } from '@/features/problem/utils/supported-languages';

// Template languages offered by the form; a failed load offers none and the
// form still saves.
const NO_LANGUAGES: SupportedLanguage[] = [];

interface ProblemFormProps {
  data: ProblemFormData;
  onChange: (data: ProblemFormData) => void;
  problemId?: number;
  attachments?: Attachment[];
}

export function ProblemForm({
  data,
  onChange,
  problemId,
  attachments,
}: ProblemFormProps) {
  const { t } = useTranslation();
  const label = useIdentifierLabel();
  const registriesQuery = useRegistries();
  // What this server registers, once loaded: a stored type it does not list
  // shows as unavailable, and nothing is cleared while the registries load
  // or after they fail.
  const registries = useQueryState(registriesQuery);
  const registered = loadedData(registries);
  const languagesQuery = useSupportedLanguages();
  // A language list that could not load says so instead of offering none.
  const languages = useQueryState(languagesQuery);
  const supportedLanguages = loadedData(languages) ?? NO_LANGUAGES;
  const [selectedLanguage, setSelectedLanguage] = useState('');
  const [newFilenameByLanguage, setNewFilenameByLanguage] = useState<
    Record<string, string>
  >({});

  // A new problem starts with the server's default checker format, once the
  // registries say what it is. A stored value is never replaced.
  useEffect(() => {
    if (!registered || data.checkerFormat) return;
    const checkerFormat = checkerFormatFor(
      data.checkerFormat,
      registered.checker_formats,
      registered.default_checker_format,
    );
    if (checkerFormat) onChange({ ...data, checkerFormat });
  }, [data, registered, onChange]);

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
    // Each registered language names its default file.
    const languageMeta = languageById.get(lang);
    if (!languageMeta) return;
    onChange({
      ...data,
      submissionFormat: {
        ...data.submissionFormat,
        [lang]: [languageMeta.defaultFilename],
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
          placeholder={t('admin.problem.titlePlaceholder')}
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
              placeholder={t('admin.problem.contentPlaceholder')}
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
            placeholder={t('admin.problem.contentPlaceholder')}
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
        <RegistrySelect
          id="problem-type"
          label={t('admin.field.problemType')}
          value={data.problemType}
          onChange={handleProblemTypeChange}
          registered={registered?.problem_types}
          name={(id) => label('problemType', id)}
        />
        <RegistrySelect
          id="checker-format"
          label={t('admin.field.checkerFormat')}
          value={data.checkerFormat}
          onChange={handleCheckerFormatChange}
          registered={registered?.checker_formats}
          name={(id) => label('checker', id)}
        />
        <RegistrySelect
          id="default-contest-type"
          label={t('admin.field.contestType')}
          value={data.defaultContestType}
          onChange={handleDefaultContestTypeChange}
          registered={registered?.contest_types}
          name={(id) => label('contestType', id)}
        />
      </div>
      {registries.status === 'error' && (
        <QueryErrorState state={registries} className="py-4" />
      )}

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
        {languages.status === 'error' && (
          <QueryErrorState state={languages} className="py-4" />
        )}

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
                        aria-label={t('common.remove')}
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
                    aria-label={t('admin.submissionFormat.filenamePlaceholder')}
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

/**
 * A field chosen from what the plugins registered: the stored value always
 * shows, marked unavailable when this server does not register it.
 */
function RegistrySelect({
  id,
  label,
  value,
  onChange,
  registered,
  name,
}: {
  id: string;
  label: string;
  value: string;
  onChange: (value: string) => void;
  /** What this server registers; undefined until the registries load. */
  registered: readonly string[] | undefined;
  name: (id: string) => string;
}) {
  const { t } = useTranslation();
  const choices = registryChoices(registered, value);
  return (
    <div className="space-y-2">
      <Label htmlFor={id}>{label}</Label>
      <Select
        value={value}
        onValueChange={onChange}
        disabled={registered === undefined || choices.length === 0}
      >
        <SelectTrigger id={id}>
          <SelectValue placeholder={t('admin.field.selectPlaceholder')} />
        </SelectTrigger>
        <SelectContent>
          {choices.map((choice) => (
            <SelectItem key={choice.value} value={choice.value}>
              {registryChoiceLabel(choice, name(choice.value), t)}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    </div>
  );
}
