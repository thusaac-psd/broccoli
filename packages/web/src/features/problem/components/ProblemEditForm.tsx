import {
  type ApiClient,
  useApiClient,
  useErrorMessage,
} from '@broccoli/web-sdk/api';
import { useAsyncSubmit } from '@broccoli/web-sdk/hooks';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Button } from '@broccoli/web-sdk/ui';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useState } from 'react';
import { useNavigate } from 'react-router';
import { toast } from 'sonner';

import { RecordLoader } from '@/components/RecordLoader';
import { AdditionalFilesSection } from '@/features/admin/components/AdditionalFilesSection';
import { AttachmentsSection } from '@/features/admin/components/AttachmentsSection';
import { ProblemForm } from '@/features/admin/components/ProblemForm';
import { TestCaseList } from '@/features/admin/components/TestCaseList';
import {
  type ProblemFormData,
  problemFormProblem,
  problemFormValues,
  problemRequestBody,
} from '@/features/admin/problem-form-values';
import {
  attachmentsQueryKey,
  fetchAttachments,
} from '@/features/problem/api/attachments';
import { changedFields } from '@/lib/changed-fields';

async function fetchProblem(apiClient: ApiClient, id: number) {
  const { data, error } = await apiClient.GET('/problems/{id}', {
    params: { path: { id } },
  });
  if (error) throw error;
  return data;
}

export function ProblemEditForm({ problemId }: { problemId: number }) {
  const apiClient = useApiClient();
  return (
    <div className="min-h-screen bg-background">
      <div className="mx-auto max-w-2xl px-4 py-8">
        <RecordLoader
          key={problemId}
          queryKey={['problem-edit', problemId]}
          queryFn={() => fetchProblem(apiClient, problemId)}
        >
          {(problem) => (
            <ProblemEditFields
              problemId={problemId}
              initial={problemFormValues(problem)}
            />
          )}
        </RecordLoader>
      </div>
    </div>
  );
}

function ProblemEditFields({
  problemId: problemIdNum,
  initial,
}: {
  problemId: number;
  initial: ProblemFormData;
}) {
  const { t } = useTranslation();
  const errorMessage = useErrorMessage();
  const navigate = useNavigate();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();
  const saving = useAsyncSubmit();

  const [formData, setFormData] = useState(initial);
  const { data: attachments = [] } = useQuery({
    queryKey: attachmentsQueryKey(problemIdNum),
    queryFn: () => fetchAttachments(apiClient, problemIdNum),
  });

  async function save() {
    const { error } = await apiClient.PATCH('/problems/{id}', {
      params: { path: { id: problemIdNum } },
      body: changedFields(
        problemRequestBody(initial),
        problemRequestBody(formData),
      ),
    });
    if (error) {
      toast.error(errorMessage(error, t('admin.editError')));
      throw error;
    }
    toast.success(t('toast.problem.updated'));
    queryClient.invalidateQueries({ queryKey: ['problem', problemIdNum] });
    navigate(`/problems/${problemIdNum}`);
  }

  function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    const problem = problemFormProblem(formData);
    if (problem) {
      toast.error(t(problem));
      return;
    }
    void saving.submit(save);
  }

  return (
    <>
      <form onSubmit={handleSubmit} className="space-y-6">
        <ProblemForm
          data={formData}
          onChange={setFormData}
          problemId={problemIdNum}
          attachments={attachments}
        />

        {/* Attachments Section */}
        <div className="space-y-4 pt-4">
          <h2 className="text-lg font-semibold">
            {t('admin.attachments.title')}
          </h2>
          <AttachmentsSection problemId={problemIdNum} />
        </div>

        {/* Additional Files Section */}
        <div className="space-y-4 pt-4">
          <h2 className="text-lg font-semibold">
            {t('admin.additionalFiles.title')}
          </h2>
          <AdditionalFilesSection problemId={problemIdNum} />
        </div>

        {/* Test Cases Section */}
        <div className="space-y-4 pt-4">
          <h2 className="text-lg font-semibold">
            {t('admin.testCases.title')}
          </h2>
          <TestCaseList problemId={problemIdNum} />
        </div>

        {/* Actions */}
        <div className="flex gap-3 pt-4">
          <Button
            type="submit"
            disabled={saving.submitting}
            className="flex-1 sm:flex-none"
          >
            {saving.submitting ? t('admin.saving') : t('admin.save')}
          </Button>
        </div>
      </form>
    </>
  );
}
