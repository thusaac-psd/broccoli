import { getErrorMessage, useApiClient } from '@broccoli/web-sdk/api';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Badge, Button, Skeleton } from '@broccoli/web-sdk/ui';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { Pencil, Plus, Trash2, Upload } from 'lucide-react';
import { useEffect, useState } from 'react';
import { useNavigate } from 'react-router';
import { toast } from 'sonner';

import { AdditionalFilesSection } from '@/features/admin/components/AdditionalFilesSection';
import { AttachmentsSection } from '@/features/admin/components/AttachmentsSection';
import {
  ProblemForm,
  type ProblemFormData,
} from '@/features/admin/components/ProblemForm';
import { TestCaseBulkUploadDialog } from '@/features/admin/components/TestCaseBulkUploadDialog';
import { TestCaseFormDialog } from '@/features/admin/components/TestCaseFormDialog';
import {
  attachmentsQueryKey,
  fetchAttachments,
} from '@/features/problem/api/attachments';

interface ProblemEditFormProps {
  problemId: number;
}

export function ProblemEditForm({ problemId }: ProblemEditFormProps) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();

  const [formData, setFormData] = useState<ProblemFormData>({
    title: '',
    content: '',
    timeLimit: 1000,
    memoryLimit: 262144,
    problemType: 'standard',
    checkerFormat: 'exact',
    defaultContestType: '',
    showTestDetails: false,
    isPublic: false,
    submissionFormat: {},
  });
  const [loading, setLoading] = useState(false);
  const [loadingData, setLoadingData] = useState(true);
  const [formDialogOpen, setFormDialogOpen] = useState(false);
  const [bulkUploadDialogOpen, setBulkUploadDialogOpen] = useState(false);
  const [editingTestCaseId, setEditingTestCaseId] = useState<
    number | undefined
  >();

  const problemIdNum = Number(problemId);
  const testCasesQueryKey = ['test-cases', problemIdNum];

  // Load problem data
  useEffect(() => {
    if (!Number.isFinite(problemIdNum)) return;

    const loadProblem = async () => {
      const { data, error } = await apiClient.GET('/problems/{id}', {
        params: { path: { id: problemIdNum } },
      });

      if (error || !data) {
        toast.error(t('error.loadFailed'));
        navigate(`/problems/${problemIdNum}`);
        return;
      }

      setFormData({
        title: data.title,
        content: data.content,
        timeLimit: data.time_limit,
        memoryLimit: data.memory_limit,
        problemType: data.problem_type,
        checkerFormat: data.checker_format,
        defaultContestType: data.default_contest_type,
        showTestDetails: data.show_test_details,
        isPublic: data.is_public,
        submissionFormat: data.submission_format ?? {},
      });
      setLoadingData(false);
    };

    loadProblem();
  }, [problemIdNum, apiClient, navigate, t]);

  const { data: testCases = [], isLoading: testCasesLoading } = useQuery({
    queryKey: testCasesQueryKey,
    queryFn: async () => {
      const { data, error } = await apiClient.GET('/problems/{id}/test-cases', {
        params: { path: { id: problemIdNum } },
      });
      if (error) throw error;
      return data;
    },
    enabled: Number.isFinite(problemIdNum),
  });

  const { data: attachments = [] } = useQuery({
    queryKey: attachmentsQueryKey(problemIdNum),
    queryFn: () => fetchAttachments(apiClient, problemIdNum),
    enabled: Number.isFinite(problemIdNum),
  });

  function handleCreateTestCase() {
    setEditingTestCaseId(undefined);
    setFormDialogOpen(true);
  }

  function handleEditTestCase(testCaseId: number) {
    setEditingTestCaseId(testCaseId);
    setFormDialogOpen(true);
  }

  async function handleDeleteTestCase(testCaseId: number) {
    if (!window.confirm(t('admin.deleteConfirm'))) return;
    const { error } = await apiClient.DELETE(
      '/problems/{id}/test-cases/{tc_id}',
      {
        params: { path: { id: problemIdNum, tc_id: testCaseId } },
      },
    );
    if (error) {
      toast.error(getErrorMessage(error, t('toast.testCase.deleteError')));
    } else {
      toast.success(t('toast.testCase.deleted'));
      queryClient.invalidateQueries({ queryKey: testCasesQueryKey });
      queryClient.invalidateQueries({ queryKey: ['problem', problemIdNum] });
      queryClient.invalidateQueries({
        queryKey: ['problem-sample-contents', problemIdNum],
      });
    }
  }

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();

    if (!formData.title.trim()) {
      toast.error(t('validation.titleRequired'));
      return;
    }
    if (!formData.content.trim()) {
      toast.error(t('validation.contentRequired'));
      return;
    }

    setLoading(true);

    const body = {
      title: formData.title,
      content: formData.content,
      time_limit: formData.timeLimit,
      memory_limit: formData.memoryLimit,
      problem_type: formData.problemType,
      checker_format: formData.checkerFormat,
      default_contest_type: formData.defaultContestType,
      show_test_details: formData.showTestDetails,
      is_public: formData.isPublic,
      submission_format:
        Object.keys(formData.submissionFormat).length > 0
          ? formData.submissionFormat
          : null,
    };

    const result = await apiClient.PATCH('/problems/{id}', {
      params: { path: { id: problemIdNum } },
      body,
    });

    setLoading(false);
    if (result.error) {
      toast.error(getErrorMessage(result.error, t('admin.editError')));
    } else {
      toast.success(t('toast.problem.updated'));
      queryClient.invalidateQueries({ queryKey: ['problem', problemIdNum] });
      navigate(`/problems/${problemIdNum}`);
    }
  }

  return (
    <div className="min-h-screen bg-background">
      <div className="mx-auto max-w-2xl px-4 py-8">
        {loadingData ? (
          <div className="space-y-6">
            <Skeleton className="h-10 w-full" />
            <Skeleton className="h-64 w-full" />
            <div className="grid grid-cols-2 gap-4">
              <Skeleton className="h-10" />
              <Skeleton className="h-10" />
            </div>
          </div>
        ) : (
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
              <div className="flex items-center justify-between">
                <h2 className="text-lg font-semibold">
                  {t('admin.testCases.title')}
                </h2>
                <div className="flex gap-2">
                  <Button
                    type="button"
                    size="sm"
                    variant="outline"
                    onClick={() => setBulkUploadDialogOpen(true)}
                  >
                    <Upload className="h-4 w-4 mr-1" />
                    {t('admin.testCases.bulkUpload.button')}
                  </Button>
                  <Button
                    type="button"
                    size="sm"
                    onClick={handleCreateTestCase}
                  >
                    <Plus className="h-4 w-4 mr-1" />
                    {t('admin.testCases.create')}
                  </Button>
                </div>
              </div>

              <div className="rounded-lg border overflow-hidden">
                {testCasesLoading ? (
                  <div className="py-8 text-center text-muted-foreground">
                    {t('admin.loading')}
                  </div>
                ) : testCases.length === 0 ? (
                  <div className="py-8 text-center text-sm text-muted-foreground">
                    {t('admin.testCases.empty')}
                  </div>
                ) : (
                  <table className="w-full text-sm">
                    <thead>
                      <tr className="border-b bg-muted/40">
                        <th className="px-3 py-2 text-left font-medium text-foreground/80 w-12">
                          #
                        </th>
                        <th className="px-3 py-2 text-left font-medium text-foreground/80">
                          {t('admin.testCases.field.input')}
                        </th>
                        <th className="px-3 py-2 text-left font-medium text-foreground/80">
                          {t('admin.testCases.field.expectedOutput')}
                        </th>
                        <th className="px-3 py-2 text-center font-medium text-foreground/80 w-20">
                          {t('admin.testCases.field.score')}
                        </th>
                        <th className="px-3 py-2 text-center font-medium text-foreground/80 w-20">
                          {t('admin.testCases.field.sample')}
                        </th>
                        <th className="px-3 py-2 w-20" />
                      </tr>
                    </thead>
                    <tbody>
                      {testCases.map((tc) => (
                        <tr
                          key={tc.id}
                          className="border-b last:border-0 hover:bg-muted/30"
                        >
                          <td className="px-3 py-2 text-muted-foreground">
                            {tc.position + 1}
                          </td>
                          <td className="px-3 py-2">
                            <code className="text-xs bg-muted px-1.5 py-0.5 rounded break-all">
                              {tc.input_preview || '-'}
                            </code>
                          </td>
                          <td className="px-3 py-2">
                            <code className="text-xs bg-muted px-1.5 py-0.5 rounded break-all">
                              {tc.output_preview || '-'}
                            </code>
                          </td>
                          <td className="px-3 py-2 text-center">{tc.score}</td>
                          <td className="px-3 py-2 text-center">
                            {tc.is_sample && (
                              <Badge variant="secondary">
                                {t('admin.testCases.sample')}
                              </Badge>
                            )}
                          </td>
                          <td className="px-3 py-2 text-right">
                            <div className="flex items-center justify-end gap-1">
                              <Button
                                type="button"
                                variant="ghost"
                                size="icon"
                                className="h-7 w-7"
                                onClick={() => handleEditTestCase(tc.id)}
                              >
                                <Pencil className="h-3.5 w-3.5" />
                              </Button>
                              <Button
                                type="button"
                                variant="ghost"
                                size="icon"
                                className="h-7 w-7 text-destructive hover:text-destructive"
                                onClick={() => handleDeleteTestCase(tc.id)}
                              >
                                <Trash2 className="h-3.5 w-3.5" />
                              </Button>
                            </div>
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                )}
              </div>
            </div>

            {/* Actions */}
            <div className="flex gap-3 pt-4">
              <Button
                type="submit"
                disabled={loading}
                className="flex-1 sm:flex-none"
              >
                {loading ? t('admin.saving') : t('admin.save')}
              </Button>
            </div>
          </form>
        )}

        {/* Test Case Form Dialog */}
        <TestCaseFormDialog
          problemId={problemIdNum}
          testCaseId={editingTestCaseId}
          open={formDialogOpen}
          onOpenChange={setFormDialogOpen}
          testCasesQueryKey={testCasesQueryKey}
        />

        {/* Test Case Bulk Upload Dialog */}
        <TestCaseBulkUploadDialog
          problemId={problemIdNum}
          open={bulkUploadDialogOpen}
          onOpenChange={setBulkUploadDialogOpen}
          testCasesQueryKey={testCasesQueryKey}
        />
      </div>
    </div>
  );
}
