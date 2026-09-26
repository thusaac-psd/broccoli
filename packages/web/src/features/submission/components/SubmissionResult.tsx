import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Slot } from '@broccoli/web-sdk/slot';
import type { Submission, SubmissionError } from '@broccoli/web-sdk/submission';
import { Timer, XCircle } from 'lucide-react';

import { ReadOnlyCodeViewer } from './ReadOnlyCodeViewer';
import { TestCaseRow } from './TestCaseRow';

interface SubmissionResultProps {
  submission?: Submission | null;
  isSubmitting?: boolean;
  error?: SubmissionError | null;
}

export function SubmissionResult({
  submission,
  isSubmitting,
  error,
}: SubmissionResultProps) {
  const { t } = useTranslation();

  // No submission yet - prompt
  if (!submission && !isSubmitting && !error) {
    return (
      <div className="flex items-center justify-center h-32 text-muted-foreground">
        {t('result.submitPrompt')}
      </div>
    );
  }

  // Submission error
  if (error && !submission) {
    return (
      <Slot name="submission-result.rejection" slotProps={{ error }}>
        <RejectionMessage error={error} />
      </Slot>
    );
  }

  // Pure spinner states: Pending, Compiling
  const status = submission?.status;
  if (
    (!submission && isSubmitting) ||
    status === 'Pending' ||
    status === 'Compiling'
  ) {
    const statusLabel =
      status === 'Compiling' ? t('result.compiling') : t('result.judging');

    return (
      <div className="flex items-center justify-center h-32">
        <div className="flex flex-col items-center gap-2">
          <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-primary" />
          <p className="text-sm text-muted-foreground">{statusLabel}</p>
        </div>
      </div>
    );
  }

  if (!submission) return null;

  const result = submission.result;
  const isRunning = status === 'Running';
  const testCases = result?.test_case_results ?? [];

  return (
    <div className="space-y-2">
      {/* Code viewer (collapsed by default) */}
      {submission.files && submission.files.length > 0 && (
        <ReadOnlyCodeViewer
          files={submission.files}
          language={submission.language}
        />
      )}

      {/* Compilation error output */}
      {status === 'CompilationError' && result?.compile_output && (
        <div className="mb-4">
          <div className="text-sm font-medium mb-1">
            {t('result.compileOutput')}
          </div>
          <pre className="text-xs bg-muted p-3 rounded-lg overflow-x-auto whitespace-pre-wrap">
            {result.compile_output}
          </pre>
        </div>
      )}

      {/* System error message */}
      {status === 'SystemError' && result?.error_message && (
        <div className="mb-4 text-sm text-destructive space-y-1">
          <div className="font-medium">{t('result.systemMessage')}</div>
          <pre className="text-xs bg-muted p-3 rounded-lg overflow-x-auto whitespace-pre-wrap">
            {result.error_message}
          </pre>
        </div>
      )}

      {/* Test case results */}
      <Slot
        name="submission-result.content"
        as="div"
        className="space-y-2"
        slotProps={{ submission, testCases }}
      >
        {testCases.length > 0
          ? testCases.map((tc, index) => (
              <TestCaseRow
                key={tc.id}
                testCase={tc}
                index={index + 1}
                status={submission.status}
              />
            ))
          : !isRunning &&
            status === 'Judged' && (
              <div className="text-center text-muted-foreground py-8">
                {t('result.noResults')}
              </div>
            )}
      </Slot>
    </div>
  );
}

function RejectionMessage({ error }: { error: SubmissionError }) {
  if (error.code === 'RATE_LIMITED') {
    return (
      <div className="flex items-center gap-2 text-amber-600 dark:text-amber-400 text-sm py-2">
        <Timer className="h-4 w-4 shrink-0" />
        <span>{error.message}</span>
      </div>
    );
  }
  return (
    <div className="flex items-center gap-2 text-destructive text-sm py-2">
      <XCircle className="h-4 w-4 shrink-0" />
      <span>{error.message}</span>
    </div>
  );
}
