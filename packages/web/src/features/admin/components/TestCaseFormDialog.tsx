import { useApiClient, useErrorMessage } from '@broccoli/web-sdk/api';
import {
  isAlreadyDone,
  useAsyncDialog,
  useIdempotentRequest,
} from '@broccoli/web-sdk/hooks';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import type { TestCase } from '@broccoli/web-sdk/problem';
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Input,
  Label,
  Separator,
  Textarea,
} from '@broccoli/web-sdk/ui';
import { useQueryClient } from '@tanstack/react-query';
import { Upload } from 'lucide-react';
import { useRef, useState } from 'react';
import { toast } from 'sonner';

import { RecordLoader } from '@/components/RecordLoader';
import { SwitchField } from '@/features/admin/components/SwitchField';
import { invalidateProblemTestCases } from '@/features/admin/hooks/test-case-queries';
import { changedFields } from '@/lib/changed-fields';
import { toastUnsettled } from '@/lib/idempotency-feedback';

interface TestCaseFormDialogProps {
  problemId: number;
  testCaseId?: number;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

interface TestCaseFormValues {
  label: string;
  input: string;
  expectedOutput: string;
  score: number;
  isSample: boolean;
  description: string;
}

const NEW_TEST_CASE: TestCaseFormValues = {
  label: '',
  input: '',
  expectedOutput: '',
  score: 0,
  isSample: false,
  description: '',
};

function testCaseFormValues(testCase: TestCase): TestCaseFormValues {
  return {
    label: testCase.label ?? '',
    input: testCase.input,
    expectedOutput: testCase.expected_output,
    score: testCase.score,
    isSample: testCase.is_sample,
    description: testCase.description ?? '',
  };
}

function testCaseFields(values: TestCaseFormValues) {
  return {
    input: values.input,
    expected_output: values.expectedOutput,
    score: values.score,
    is_sample: values.isSample,
    description: values.description || null,
  };
}

function testCaseUpdateBody(values: TestCaseFormValues) {
  return { ...testCaseFields(values), label: values.label.trim() || null };
}

export function TestCaseFormDialog({
  problemId,
  testCaseId,
  open,
  onOpenChange,
}: TestCaseFormDialogProps) {
  const { t } = useTranslation();
  const apiClient = useApiClient();
  const dialog = useAsyncDialog({ open, onOpenChange });
  const isEdit = testCaseId !== undefined;

  return (
    <Dialog open={dialog.open} onOpenChange={dialog.onOpenChange}>
      <DialogContent className="max-w-2xl max-h-[90vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>
            {isEdit ? t('admin.testCases.edit') : t('admin.testCases.create')}
          </DialogTitle>
          <DialogDescription>
            {isEdit ? '' : t('admin.testCases.createDesc')}
          </DialogDescription>
        </DialogHeader>

        {isEdit ? (
          <RecordLoader
            key={testCaseId}
            queryKey={['test-case-edit', problemId, testCaseId]}
            queryFn={async () => {
              const { data, error } = await apiClient.GET(
                '/problems/{id}/test-cases/{tc_id}',
                { params: { path: { id: problemId, tc_id: testCaseId } } },
              );
              if (error) throw error;
              return data;
            }}
          >
            {(testCase) => (
              <TestCaseForm
                problemId={problemId}
                testCaseId={testCaseId}
                initial={testCaseFormValues(testCase)}
                dialog={dialog}
              />
            )}
          </RecordLoader>
        ) : (
          <TestCaseForm
            problemId={problemId}
            initial={NEW_TEST_CASE}
            dialog={dialog}
          />
        )}
      </DialogContent>
    </Dialog>
  );
}

function TestCaseForm({
  problemId,
  testCaseId,
  initial,
  dialog,
}: {
  problemId: number;
  /** The test case being edited; creates a new one when absent. */
  testCaseId?: number;
  initial: TestCaseFormValues;
  dialog: ReturnType<typeof useAsyncDialog>;
}) {
  const { t } = useTranslation();
  const errorMessage = useErrorMessage();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();
  const sendIdempotent = useIdempotentRequest();
  const [values, setValues] = useState(initial);
  const isEdit = testCaseId !== undefined;

  const set =
    <K extends keyof TestCaseFormValues>(field: K) =>
    (value: TestCaseFormValues[K]) =>
      setValues((prev) => ({ ...prev, [field]: value }));

  async function save() {
    const createBody = {
      ...testCaseFields(values),
      label: values.label.trim(),
    };

    const result = isEdit
      ? await apiClient.PATCH('/problems/{id}/test-cases/{tc_id}', {
          params: { path: { id: problemId, tc_id: testCaseId } },
          body: changedFields(
            testCaseUpdateBody(initial),
            testCaseUpdateBody(values),
          ),
        })
      : await sendIdempotent({ problemId, body: createBody }, (key) =>
          apiClient.POST('/problems/{id}/test-cases', {
            headers: { 'Idempotency-Key': key },
            params: { path: { id: problemId } },
            body: createBody,
          }),
        );

    const refetch = () => invalidateProblemTestCases(queryClient, problemId);
    // Unsettled: the form stays open, so if the test case is not in the
    // list it can be sent again as it is.
    if (toastUnsettled(result.error, t, { refetch })) throw result.error;
    if (result.error && !isAlreadyDone(result.error)) {
      toast.error(
        errorMessage(
          result.error,
          isEdit ? t('admin.editError') : t('admin.createError'),
        ),
      );
      throw result.error;
    }
    toast.success(
      isEdit ? t('toast.testCase.updated') : t('toast.testCase.created'),
    );
    refetch();
  }

  function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    void dialog.submit(save);
  }

  return (
    <form onSubmit={handleSubmit} className="space-y-4">
      <fieldset disabled={dialog.submitting} className="space-y-4">
        <TestDataField
          id="tc-input"
          label={t('admin.testCases.field.input')}
          value={values.input}
          onChange={set('input')}
          placeholder={t('admin.testCases.inputPlaceholder')}
        />
        <TestDataField
          id="tc-expected-output"
          label={t('admin.testCases.field.expectedOutput')}
          value={values.expectedOutput}
          onChange={set('expectedOutput')}
          placeholder={t('admin.testCases.outputPlaceholder')}
        />

        <div className="grid grid-cols-1 gap-4 sm:grid-cols-3">
          <div className="space-y-2">
            <Label htmlFor="tc-label">{t('admin.testCases.field.label')}</Label>
            <Input
              id="tc-label"
              value={values.label}
              onChange={(e) => set('label')(e.target.value)}
              maxLength={64}
              required={!isEdit}
              placeholder={t('admin.testCases.labelPlaceholder')}
              className="font-mono"
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="tc-score">{t('admin.testCases.field.score')}</Label>
            <Input
              id="tc-score"
              type="number"
              min={0}
              max={10000}
              value={values.score}
              onChange={(e) => set('score')(Number(e.target.value))}
              required
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="tc-description">
              {t('admin.testCases.field.description')}
            </Label>
            <Textarea
              id="tc-description"
              value={values.description}
              onChange={(e) => set('description')(e.target.value)}
              maxLength={4096}
              rows={3}
              placeholder={t('admin.testCases.descriptionPlaceholder')}
            />
          </div>
        </div>

        <Separator />

        <div className="space-y-3">
          <Label className="text-sm text-muted-foreground">
            {t('admin.field.options')}
          </Label>
          <SwitchField
            id="tc-is-sample"
            label={t('admin.testCases.field.isSample')}
            checked={values.isSample}
            onCheckedChange={set('isSample')}
          />
        </div>
      </fieldset>

      <DialogFooter>
        <Button type="submit" disabled={dialog.submitting}>
          {dialog.submitting
            ? t('admin.saving')
            : isEdit
              ? t('admin.save')
              : t('admin.testCases.create')}
        </Button>
      </DialogFooter>
    </form>
  );
}

/** Uploaded text longer than this is not shown in the textarea. */
const MAX_SHOWN_UPLOAD_LENGTH = 100;

/**
 * A test data textarea that can also be filled from a file. A long uploaded
 * file is kept out of the textarea; clicking its summary clears it.
 */
function TestDataField({
  id,
  label,
  value,
  onChange,
  placeholder,
}: {
  id: string;
  label: string;
  value: string;
  onChange: (value: string) => void;
  placeholder: string;
}) {
  const { t } = useTranslation();
  const fileRef = useRef<HTMLInputElement>(null);
  const [uploaded, setUploaded] = useState(false);

  const handleFile = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const inputEl = e.currentTarget;
    const file = inputEl.files?.[0];
    if (!file) return;
    try {
      const content = await file.text();
      onChange(content);
      setUploaded(true);
    } catch {
      toast.error(t('admin.testCases.fileReadError'));
    } finally {
      inputEl.value = '';
    }
  };

  return (
    <div className="space-y-2">
      <Label htmlFor={id}>{label}</Label>
      <input
        ref={fileRef}
        type="file"
        className="hidden"
        onChange={handleFile}
      />
      <Button
        type="button"
        variant="outline"
        onClick={() => fileRef.current?.click()}
        className="w-full"
      >
        <Upload className="h-4 w-4 mr-2" />
        {t('admin.testCases.uploadFile')}
      </Button>
      {uploaded && value.length > MAX_SHOWN_UPLOAD_LENGTH ? (
        <Button
          type="button"
          variant="outline"
          className="w-full"
          onClick={() => {
            onChange('');
            setUploaded(false);
          }}
        >
          {t('admin.testCases.uploadedHidden', { length: value.length })}
        </Button>
      ) : (
        <Textarea
          id={id}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          rows={6}
          className="font-mono text-sm"
          placeholder={placeholder}
        />
      )}
    </div>
  );
}
