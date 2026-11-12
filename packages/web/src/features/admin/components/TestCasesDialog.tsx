import { useApiClient } from '@broccoli/web-sdk/api';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import type { ProblemSummary } from '@broccoli/web-sdk/problem';
import {
  Badge,
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@broccoli/web-sdk/ui';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { Pencil, Plus, Trash2 } from 'lucide-react';
import { useState } from 'react';
import { toast } from 'sonner';

import { TestCaseFormDialog } from '@/features/admin/components/TestCaseFormDialog';

interface TestCasesDialogProps {
  problem: ProblemSummary;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

export function TestCasesDialog({
  problem,
  open,
  onOpenChange,
}: TestCasesDialogProps) {
  const { t } = useTranslation();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();

  const [formDialogOpen, setFormDialogOpen] = useState(false);
  const [editingTestCaseId, setEditingTestCaseId] = useState<
    number | undefined
  >();
  const [errorMsg, setErrorMsg] = useState('');

  const testCasesKey = ['test-cases', problem.id];

  const { data: testCases = [], isLoading } = useQuery({
    queryKey: testCasesKey,
    queryFn: async () => {
      const { data, error } = await apiClient.GET('/problems/{id}/test-cases', {
        params: { path: { id: problem.id } },
      });
      if (error) throw error;
      return data;
    },
    enabled: open,
  });

  function handleCreate() {
    setErrorMsg('');
    setEditingTestCaseId(undefined);
    setFormDialogOpen(true);
  }

  function handleEdit(testCaseId: number) {
    setErrorMsg('');
    setEditingTestCaseId(testCaseId);
    setFormDialogOpen(true);
  }

  async function handleDelete(testCaseId: number) {
    if (!window.confirm(t('admin.deleteConfirm'))) return;
    setErrorMsg('');
    const { error } = await apiClient.DELETE(
      '/problems/{id}/test-cases/{tc_id}',
      {
        params: { path: { id: problem.id, tc_id: testCaseId } },
      },
    );
    if (error) {
      toast.error(t('toast.testCase.deleteError'));
    } else {
      toast.success(t('toast.testCase.deleted'));
      queryClient.invalidateQueries({ queryKey: testCasesKey });
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-3xl max-h-[90vh] flex flex-col">
        <DialogHeader>
          <DialogTitle>{t('admin.testCases.title')}</DialogTitle>
          <DialogDescription>{problem.title}</DialogDescription>
        </DialogHeader>

        <div className="flex justify-end">
          <Button size="sm" onClick={handleCreate}>
            <Plus className="h-4 w-4 mr-1" />
            {t('admin.testCases.create')}
          </Button>
        </div>

        {errorMsg && <p className="text-sm text-destructive">{errorMsg}</p>}

        <div className="overflow-y-auto flex-1 rounded-md border">
          {isLoading ? (
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
                          variant="ghost"
                          size="icon"
                          className="h-7 w-7"
                          onClick={() => handleEdit(tc.id)}
                        >
                          <Pencil className="h-3.5 w-3.5" />
                        </Button>
                        <Button
                          variant="ghost"
                          size="icon"
                          className="h-7 w-7 text-destructive hover:text-destructive"
                          onClick={() => handleDelete(tc.id)}
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
          problemId={problem.id}
          testCaseId={editingTestCaseId}
          open={formDialogOpen}
          onOpenChange={setFormDialogOpen}
        />
      </DialogContent>
    </Dialog>
  );
}
