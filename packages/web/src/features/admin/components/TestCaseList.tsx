import { useApiClient, useErrorMessage } from '@broccoli/web-sdk/api';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Badge, Button, useConfirmDialog } from '@broccoli/web-sdk/ui';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { Pencil, Plus, Trash2, Upload } from 'lucide-react';
import { useState } from 'react';
import { toast } from 'sonner';

import { TestCaseBulkUploadDialog } from '@/features/admin/components/TestCaseBulkUploadDialog';
import { TestCaseFormDialog } from '@/features/admin/components/TestCaseFormDialog';
import {
  invalidateProblemTestCases,
  testCasesQueryKey,
} from '@/features/admin/hooks/test-case-queries';

/**
 * A problem's test cases with their create, edit, bulk upload and delete
 * actions.
 */
export function TestCaseList({ problemId }: { problemId: number }) {
  const { t } = useTranslation();
  const errorMessage = useErrorMessage();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();
  const [confirm, confirmDialog] = useConfirmDialog();
  const [formOpen, setFormOpen] = useState(false);
  const [bulkUploadOpen, setBulkUploadOpen] = useState(false);
  const [editingId, setEditingId] = useState<number | undefined>();

  const {
    data: testCases = [],
    isLoading,
    error,
    refetch,
  } = useQuery({
    queryKey: testCasesQueryKey(problemId),
    queryFn: async () => {
      const { data, error } = await apiClient.GET('/problems/{id}/test-cases', {
        params: { path: { id: problemId } },
      });
      if (error) throw error;
      return data;
    },
  });

  function openForm(testCaseId?: number) {
    setEditingId(testCaseId);
    setFormOpen(true);
  }

  function confirmDelete(testCaseId: number) {
    confirm({
      title: t('admin.deleteConfirm'),
      confirmLabel: t('admin.delete'),
      destructive: true,
      failureMessage: t('toast.testCase.deleteError'),
      onConfirm: async () => {
        const { error } = await apiClient.DELETE(
          '/problems/{id}/test-cases/{tc_id}',
          { params: { path: { id: problemId, tc_id: testCaseId } } },
        );
        if (error) throw error;
        toast.success(t('toast.testCase.deleted'));
        invalidateProblemTestCases(queryClient, problemId);
      },
    });
  }

  return (
    <div className="space-y-3">
      <div className="flex justify-end gap-2">
        <Button
          type="button"
          size="sm"
          variant="outline"
          onClick={() => setBulkUploadOpen(true)}
        >
          <Upload className="h-4 w-4 mr-1" />
          {t('admin.testCases.bulkUpload.button')}
        </Button>
        <Button type="button" size="sm" onClick={() => openForm()}>
          <Plus className="h-4 w-4 mr-1" />
          {t('admin.testCases.create')}
        </Button>
      </div>

      <div className="overflow-y-auto rounded-md border">
        {isLoading ? (
          <div className="py-8 text-center text-muted-foreground">
            {t('common.loading')}
          </div>
        ) : error ? (
          <div role="alert" className="space-y-3 py-8 text-center">
            <p className="text-sm text-destructive">
              {errorMessage(error, t('common.loadFailed'))}
            </p>
            <Button variant="outline" size="sm" onClick={() => refetch()}>
              {t('common.retry')}
            </Button>
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
                  {t('admin.testCases.field.position')}
                </th>
                <th className="px-3 py-2 text-left font-medium text-foreground/80 w-32">
                  {t('admin.testCases.field.label')}
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
                    <code className="text-xs font-semibold text-foreground/90">
                      {tc.label || `#${tc.id}`}
                    </code>
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
                        aria-label={t('admin.edit')}
                        onClick={() => openForm(tc.id)}
                      >
                        <Pencil className="h-3.5 w-3.5" />
                      </Button>
                      <Button
                        type="button"
                        variant="ghost"
                        size="icon"
                        className="h-7 w-7 text-destructive hover:text-destructive"
                        aria-label={t('admin.delete')}
                        onClick={() => confirmDelete(tc.id)}
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

      <TestCaseFormDialog
        problemId={problemId}
        testCaseId={editingId}
        open={formOpen}
        onOpenChange={setFormOpen}
      />
      <TestCaseBulkUploadDialog
        problemId={problemId}
        open={bulkUploadOpen}
        onOpenChange={setBulkUploadOpen}
      />
      {confirmDialog}
    </div>
  );
}
