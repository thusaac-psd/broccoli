import { getErrorMessage, useApiClient } from '@broccoli/web-sdk/api';
import type { ContestProblem } from '@broccoli/web-sdk/contest';
import { useIdempotencyKey } from '@broccoli/web-sdk/hooks';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import type { ProblemSummary } from '@broccoli/web-sdk/problem';
import {
  Badge,
  Button,
  DataTable,
  type DataTableColumn,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@broccoli/web-sdk/ui';
import { formatDateTime } from '@broccoli/web-sdk/utils';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import {
  FileCode,
  Image,
  List,
  MoreHorizontal,
  Paperclip,
  Pencil,
  Plus,
  Settings,
  Trash2,
} from 'lucide-react';
import { useEffect, useState } from 'react';
import { Link } from 'react-router';
import { toast } from 'sonner';

import { ResourceConfigDialog } from '@/components/config';
import { AdditionalFilesDialog } from '@/features/admin/components/AdditionalFilesDialog';
import { AttachmentsDialog } from '@/features/admin/components/AttachmentsDialog';
import { CheckerSourceDialog } from '@/features/admin/components/CheckerSourceDialog';
import {
  ProblemForm,
  type ProblemFormData,
} from '@/features/admin/components/ProblemForm';
import { TestCasesDialog } from '@/features/admin/components/TestCasesDialog';
import { fetchContestProblems } from '@/features/contest/api/fetch-contest-problems';
import {
  attachmentsQueryKey,
  fetchAttachments,
} from '@/features/problem/api/attachments';
import { fetchProblems } from '@/features/problem/api/fetch-problems';
import { useTableSearchParams } from '@/hooks/use-table-search-params';

// -- Helpers --

// A-Z, then AA-AZ, BA-BZ, ..., ZZ (max 702)
function nextProblemLabel(usedLabels: Set<string>): string {
  for (let i = 0; i < 26; i++) {
    const label = String.fromCharCode(65 + i);
    if (!usedLabels.has(label)) return label;
  }
  for (let i = 0; i < 26; i++) {
    for (let j = 0; j < 26; j++) {
      const label = String.fromCharCode(65 + i) + String.fromCharCode(65 + j);
      if (!usedLabels.has(label)) return label;
    }
  }
  return '';
}

// -- Problem Form Dialog --

export function ProblemFormDialog({
  problem,
  contestId,
  open,
  onOpenChange,
}: {
  problem?: ProblemSummary;
  /**
   * When set, a newly created problem is also attached to this contest (with
   * the next free label) so it shows up in the contest-scoped problem list.
   */
  contestId?: number;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const isEdit = !!problem;

  const [title, setTitle] = useState('');
  const [content, setContent] = useState('');
  const [timeLimit, setTimeLimit] = useState(1000);
  const [memoryLimit, setMemoryLimit] = useState(262144);
  const [problemType, setProblemType] = useState('standard');
  const [checkerFormat, setCheckerFormat] = useState('exact');
  const [defaultContestType, setDefaultContestType] = useState('');
  const [showTestDetails, setShowTestDetails] = useState(false);
  const [isPublic, setIsPublic] = useState(false);
  const [submissionFormat, setSubmissionFormat] = useState<
    Record<string, string[]>
  >({});
  const [loading, setLoading] = useState(false);
  const [loadingData, setLoadingData] = useState(false);
  const apiClient = useApiClient();
  const { getKey, resetKey } = useIdempotencyKey();

  const { data: attachments = [] } = useQuery({
    queryKey: attachmentsQueryKey(problem?.id ?? 0),
    queryFn: () => fetchAttachments(apiClient, problem!.id),
    enabled: isEdit && open,
  });

  const formData: ProblemFormData = {
    title,
    content,
    timeLimit,
    memoryLimit,
    problemType,
    checkerFormat,
    defaultContestType,
    showTestDetails,
    isPublic,
    submissionFormat,
  };

  const handleFormChange = (data: ProblemFormData) => {
    setTitle(data.title);
    setContent(data.content);
    setTimeLimit(data.timeLimit);
    setMemoryLimit(data.memoryLimit);
    setProblemType(data.problemType);
    setCheckerFormat(data.checkerFormat);
    setDefaultContestType(data.defaultContestType);
    setShowTestDetails(data.showTestDetails);
    setIsPublic(data.isPublic);
    setSubmissionFormat(data.submissionFormat);
  };

  useEffect(() => {
    if (!open) return;
    if (problem) {
      setLoadingData(true);
      apiClient
        .GET('/problems/{id}', { params: { path: { id: problem.id } } })
        .then(({ data, error }) => {
          setLoadingData(false);
          if (error || !data) return;
          setTitle(data.title);
          setContent(data.content);
          setTimeLimit(data.time_limit);
          setMemoryLimit(data.memory_limit);
          setProblemType(data.problem_type);
          setCheckerFormat(data.checker_format);
          setDefaultContestType(data.default_contest_type);
          setShowTestDetails(data.show_test_details);
          setIsPublic(data.is_public);
          setSubmissionFormat(data.submission_format ?? {});
        });
    } else {
      setTitle('');
      setContent('');
      setTimeLimit(1000);
      setMemoryLimit(262144);
      setProblemType('standard');
      setCheckerFormat('exact');
      setDefaultContestType('');
      setShowTestDetails(false);
      setIsPublic(false);
      setSubmissionFormat({});
    }
  }, [apiClient, open, problem]);

  async function attachToContest(problemId: number): Promise<boolean> {
    if (!contestId) return false;
    const { data: existing, error: listError } = await apiClient.GET(
      '/contests/{id}/problems',
      { params: { path: { id: contestId } } },
    );
    if (listError || !existing) return false;
    const label = nextProblemLabel(
      new Set(existing.map((p: ContestProblem) => p.label)),
    );
    if (!label) return false;
    const { error } = await apiClient.POST('/contests/{id}/problems', {
      headers: { 'Idempotency-Key': getKey() },
      params: { path: { id: contestId } },
      body: { problem_id: problemId, label },
    });
    if (error) return false;
    resetKey();
    return true;
  }

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();

    if (!title.trim()) {
      toast.error(t('validation.titleRequired'));
      return;
    }
    if (!content.trim()) {
      toast.error(t('validation.contentRequired'));
      return;
    }
    if (!defaultContestType) {
      toast.error(t('validation.contestTypeRequired'));
      return;
    }

    setLoading(true);

    const body = {
      title,
      content,
      time_limit: timeLimit,
      memory_limit: memoryLimit,
      problem_type: problemType,
      checker_format: checkerFormat,
      default_contest_type: defaultContestType,
      show_test_details: showTestDetails,
      is_public: isPublic,
      submission_format:
        Object.keys(submissionFormat).length > 0 ? submissionFormat : null,
    };

    const result = isEdit
      ? await apiClient.PATCH('/problems/{id}', {
          params: { path: { id: problem!.id } },
          body,
        })
      : await apiClient.POST('/problems', {
          headers: { 'Idempotency-Key': getKey() },
          body,
        });

    if (result.error) {
      setLoading(false);
      toast.error(
        getErrorMessage(
          result.error,
          isEdit ? t('admin.editError') : t('admin.createError'),
        ),
      );
      return;
    }

    if (!isEdit) resetKey();

    let attachFailed = false;
    if (!isEdit && contestId && result.data) {
      attachFailed = !(await attachToContest(result.data.id));
    }
    setLoading(false);

    if (attachFailed) {
      toast.warning(t('toast.problem.createdNotAttached'));
    } else {
      toast.success(
        isEdit ? t('toast.problem.updated') : t('toast.problem.created'),
      );
    }
    queryClient.invalidateQueries({ queryKey: ['admin-problems'] });
    if (contestId) {
      queryClient.invalidateQueries({
        queryKey: ['contest-problems', contestId],
      });
    }
    onOpenChange(false);
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-2xl max-h-[90vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>
            {isEdit ? t('admin.editProblem') : t('admin.createProblem')}
          </DialogTitle>
          <DialogDescription>
            {isEdit
              ? ''
              : contestId
                ? t('admin.createProblemInContestDesc')
                : t('admin.createProblemDesc')}
          </DialogDescription>
        </DialogHeader>

        {loadingData ? (
          <div className="py-8 text-center text-muted-foreground">
            Loading...
          </div>
        ) : (
          <form onSubmit={handleSubmit} className="space-y-4">
            <ProblemForm
              data={formData}
              onChange={handleFormChange}
              problemId={isEdit ? problem!.id : undefined}
              attachments={isEdit ? attachments : undefined}
            />
            <DialogFooter>
              <Button type="submit" disabled={loading}>
                {loading
                  ? t('admin.saving')
                  : isEdit
                    ? t('admin.save')
                    : t('admin.createProblem')}
              </Button>
            </DialogFooter>
          </form>
        )}
      </DialogContent>
    </Dialog>
  );
}

// -- Row actions menu (shared between global and contest column sets) --

function ProblemActionsMenu({
  onManageTestCases,
  onManageAdditionalFiles,
  onManageCheckerSource,
  onManageAttachments,
  onConfigure,
  onEdit,
  destructiveLabel,
  onDestructive,
}: {
  onManageTestCases: () => void;
  onManageAdditionalFiles: () => void;
  onManageCheckerSource: () => void;
  onManageAttachments: () => void;
  onConfigure: () => void;
  onEdit: () => void;
  destructiveLabel: string;
  onDestructive: () => void;
}) {
  const { t } = useTranslation();
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="ghost" size="icon" className="h-7 w-7">
          <MoreHorizontal className="h-4 w-4" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuItem onClick={onManageTestCases}>
          <List className="h-4 w-4" />
          {t('admin.manageTestCases')}
        </DropdownMenuItem>
        <DropdownMenuItem onClick={onManageAdditionalFiles}>
          <Paperclip className="h-4 w-4" />
          {t('admin.manageAdditionalFiles')}
        </DropdownMenuItem>
        <DropdownMenuItem onClick={onManageCheckerSource}>
          <FileCode className="h-4 w-4" />
          {t('admin.manageCheckerSource')}
        </DropdownMenuItem>
        <DropdownMenuItem onClick={onManageAttachments}>
          <Image className="h-4 w-4" />
          {t('admin.manageAttachments')}
        </DropdownMenuItem>
        <DropdownMenuItem onClick={onConfigure}>
          <Settings className="h-4 w-4" />
          {t('admin.configure')}
        </DropdownMenuItem>
        <DropdownMenuItem onClick={onEdit}>
          <Pencil className="h-4 w-4" />
          {t('admin.edit')}
        </DropdownMenuItem>
        <DropdownMenuSeparator />
        <DropdownMenuItem
          className="text-destructive focus:text-destructive"
          onClick={onDestructive}
        >
          <Trash2 className="h-4 w-4" />
          {destructiveLabel}
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

// -- Column hook --

function useProblemColumns({
  onEdit,
  onDelete,
  onManageTestCases,
  onManageAdditionalFiles,
  onManageAttachments,
  onManageCheckerSource,
  onConfigure,
}: {
  onEdit: (problem: ProblemSummary) => void;
  onDelete: (problem: ProblemSummary) => void;
  onManageTestCases: (problem: ProblemSummary) => void;
  onManageAdditionalFiles: (problem: ProblemSummary) => void;
  onManageAttachments: (problem: ProblemSummary) => void;
  onManageCheckerSource: (problem: ProblemSummary) => void;
  onConfigure: (problem: ProblemSummary) => void;
}): DataTableColumn<ProblemSummary>[] {
  const { t, locale } = useTranslation();
  return [
    { accessorKey: 'id', header: '#', size: 60 },
    {
      accessorKey: 'title',
      header: t('admin.field.title'),
      sortKey: 'title',
      cell: ({ row }) => (
        <Link
          to={`/problems/${row.original.id}`}
          className="font-medium hover:text-primary hover:underline"
        >
          {row.original.title}
        </Link>
      ),
    },
    {
      accessorKey: 'time_limit',
      header: t('admin.field.timeLimit'),
      size: 120,
      cell: ({ row }) => (
        <span className="text-muted-foreground">
          {row.original.time_limit}ms
        </span>
      ),
    },
    {
      accessorKey: 'memory_limit',
      header: t('admin.field.memoryLimit'),
      size: 120,
      cell: ({ row }) => (
        <span className="text-muted-foreground">
          {(row.original.memory_limit / 1024).toFixed(0)}MB
        </span>
      ),
    },
    {
      accessorKey: 'problem_type',
      header: t('admin.field.problemType'),
      size: 120,
      cell: ({ row }) => (
        <Badge variant="outline">{row.original.problem_type}</Badge>
      ),
    },
    {
      accessorKey: 'checker_format',
      header: t('admin.field.checkerFormat'),
      size: 120,
      cell: ({ row }) => (
        <span className="text-muted-foreground">
          {row.original.checker_format}
        </span>
      ),
    },
    {
      accessorKey: 'created_at',
      header: t('admin.field.createdAt'),
      size: 180,
      sortKey: 'created_at',
      cell: ({ row }) => (
        <span className="text-muted-foreground">
          {formatDateTime(row.original.created_at, locale)}
        </span>
      ),
    },
    {
      id: 'actions',
      header: '',
      size: 50,
      cell: ({ row }) => (
        <ProblemActionsMenu
          onManageTestCases={() => onManageTestCases(row.original)}
          onManageAdditionalFiles={() => onManageAdditionalFiles(row.original)}
          onManageCheckerSource={() => onManageCheckerSource(row.original)}
          onManageAttachments={() => onManageAttachments(row.original)}
          onConfigure={() => onConfigure(row.original)}
          onEdit={() => onEdit(row.original)}
          destructiveLabel={t('admin.delete')}
          onDestructive={() => onDelete(row.original)}
        />
      ),
    },
  ];
}

// -- Contest-scoped column hook --
//
// Contest rows are ContestProblem relationship records (label, position,
// problem_id, problem_title), not full ProblemSummary objects, so they get
// their own column set. The destructive action detaches the problem from the
// contest instead of deleting it platform-wide.

function useContestProblemColumns({
  contestId,
  onEdit,
  onDetach,
  onManageTestCases,
  onManageAdditionalFiles,
  onManageAttachments,
  onManageCheckerSource,
  onConfigure,
}: {
  contestId: number;
  onEdit: (row: ContestProblem) => void;
  onDetach: (row: ContestProblem) => void;
  onManageTestCases: (row: ContestProblem) => void;
  onManageAdditionalFiles: (row: ContestProblem) => void;
  onManageAttachments: (row: ContestProblem) => void;
  onManageCheckerSource: (row: ContestProblem) => void;
  onConfigure: (row: ContestProblem) => void;
}): DataTableColumn<ContestProblem>[] {
  const { t } = useTranslation();
  return [
    {
      accessorKey: 'label',
      header: t('admin.field.label'),
      size: 60,
      cell: ({ row }) => (
        <span className="font-medium">{row.original.label}</span>
      ),
    },
    { accessorKey: 'problem_id', header: '#', size: 60 },
    {
      accessorKey: 'problem_title',
      header: t('admin.field.title'),
      cell: ({ row }) => (
        <Link
          to={`/contests/${contestId}/problems/${row.original.problem_id}`}
          className="font-medium hover:text-primary hover:underline"
        >
          {row.original.problem_title}
        </Link>
      ),
    },
    {
      id: 'actions',
      header: '',
      size: 50,
      cell: ({ row }) => (
        <ProblemActionsMenu
          onManageTestCases={() => onManageTestCases(row.original)}
          onManageAdditionalFiles={() => onManageAdditionalFiles(row.original)}
          onManageCheckerSource={() => onManageCheckerSource(row.original)}
          onManageAttachments={() => onManageAttachments(row.original)}
          onConfigure={() => onConfigure(row.original)}
          onEdit={() => onEdit(row.original)}
          destructiveLabel={t('admin.removeFromContest')}
          onDestructive={() => onDetach(row.original)}
        />
      ),
    },
  ];
}

// -- Problems Tab --

export function AdminProblemsTab({ contestId }: { contestId?: number }) {
  const { t } = useTranslation();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();
  const table = useTableSearchParams({
    defaultSortBy: 'created_at',
    defaultSortOrder: 'desc',
  });

  const [problemDialogOpen, setProblemDialogOpen] = useState(false);
  const [editingProblem, setEditingProblem] = useState<
    ProblemSummary | undefined
  >();
  const [testCasesDialogOpen, setTestCasesDialogOpen] = useState(false);
  const [managingProblem, setManagingProblem] = useState<
    ProblemSummary | undefined
  >();
  const [additionalFilesDialogOpen, setAdditionalFilesDialogOpen] =
    useState(false);
  const [additionalFilesProblem, setAdditionalFilesProblem] = useState<
    ProblemSummary | undefined
  >();
  const [attachmentsDialogOpen, setAttachmentsDialogOpen] = useState(false);
  const [attachmentsProblem, setAttachmentsProblem] = useState<
    ProblemSummary | undefined
  >();
  const [checkerSourceDialogOpen, setCheckerSourceDialogOpen] = useState(false);
  const [checkerSourceProblem, setCheckerSourceProblem] = useState<
    ProblemSummary | undefined
  >();
  const [configDialogOpen, setConfigDialogOpen] = useState(false);
  const [configProblem, setConfigProblem] = useState<
    ProblemSummary | undefined
  >();
  const [configContestProblem, setConfigContestProblem] = useState<
    ContestProblem | undefined
  >();

  // The cache key must include every input of the fetch: the global list and
  // each contest's list are different datasets and must never share an entry.
  const problemsQueryKey = contestId
    ? ['admin-problems', 'contest', String(contestId)]
    : ['admin-problems'];

  function handleCreateProblem() {
    setEditingProblem(undefined);
    setProblemDialogOpen(true);
  }

  function handleEditProblem(problem: ProblemSummary) {
    setEditingProblem(problem);
    setProblemDialogOpen(true);
  }

  function handleManageTestCases(problem: ProblemSummary) {
    setManagingProblem(problem);
    setTestCasesDialogOpen(true);
  }

  function handleManageAdditionalFiles(problem: ProblemSummary) {
    setAdditionalFilesProblem(problem);
    setAdditionalFilesDialogOpen(true);
  }

  function handleManageAttachments(problem: ProblemSummary) {
    setAttachmentsProblem(problem);
    setAttachmentsDialogOpen(true);
  }

  function handleManageCheckerSource(problem: ProblemSummary) {
    setCheckerSourceProblem(problem);
    setCheckerSourceDialogOpen(true);
  }

  function handleConfigure(problem: ProblemSummary) {
    setConfigProblem(problem);
    setConfigDialogOpen(true);
  }

  async function handleDeleteProblem(problem: ProblemSummary) {
    if (!window.confirm(t('admin.deleteConfirm'))) return;
    const { error } = await apiClient.DELETE('/problems/{id}', {
      params: { path: { id: problem.id } },
    });
    if (error) {
      toast.error(getErrorMessage(error, t('toast.problem.deleteError')));
    } else {
      toast.success(t('toast.problem.deleted'));
      // Prefix invalidation: refreshes the global list and every
      // contest-scoped list, since the problem may appear in any of them.
      queryClient.invalidateQueries({ queryKey: ['admin-problems'] });
    }
  }

  // In contest mode rows only carry the contest relationship; the management
  // dialogs need a full ProblemSummary, so resolve it on demand.
  async function resolveProblem(
    problemId: number,
  ): Promise<ProblemSummary | undefined> {
    const { data, error } = await apiClient.GET('/problems/{id}', {
      params: { path: { id: problemId } },
    });
    if (error || !data) {
      toast.error(getErrorMessage(error, t('toast.problem.loadError')));
      return undefined;
    }
    return data;
  }

  function forContestRow(open: (problem: ProblemSummary) => void) {
    return async (row: ContestProblem) => {
      const problem = await resolveProblem(row.problem_id);
      if (problem) open(problem);
    };
  }

  // Detach the problem from this contest; never deletes the problem itself.
  async function handleDetachProblem(row: ContestProblem) {
    if (!contestId) return;
    if (!window.confirm(t('admin.removeFromContestConfirm'))) return;
    const { error } = await apiClient.DELETE(
      '/contests/{id}/problems/{problem_id}',
      {
        params: { path: { id: contestId, problem_id: row.problem_id } },
      },
    );
    if (error) {
      toast.error(getErrorMessage(error, t('toast.problem.removeError')));
    } else {
      toast.success(t('toast.problem.removed'));
      queryClient.invalidateQueries({ queryKey: problemsQueryKey });
      queryClient.invalidateQueries({
        queryKey: ['contest-problems', contestId],
      });
    }
  }

  function handleConfigureContestProblem(row: ContestProblem) {
    setConfigContestProblem(row);
    setConfigDialogOpen(true);
  }

  const columns = useProblemColumns({
    onEdit: handleEditProblem,
    onDelete: handleDeleteProblem,
    onManageTestCases: handleManageTestCases,
    onManageAdditionalFiles: handleManageAdditionalFiles,
    onManageAttachments: handleManageAttachments,
    onManageCheckerSource: handleManageCheckerSource,
    onConfigure: handleConfigure,
  });

  const contestColumns = useContestProblemColumns({
    contestId: contestId ?? 0,
    onEdit: forContestRow(handleEditProblem),
    onDetach: handleDetachProblem,
    onManageTestCases: forContestRow(handleManageTestCases),
    onManageAdditionalFiles: forContestRow(handleManageAdditionalFiles),
    onManageAttachments: forContestRow(handleManageAttachments),
    onManageCheckerSource: forContestRow(handleManageCheckerSource),
    onConfigure: handleConfigureContestProblem,
  });

  const createButton = (
    <Button size="sm" onClick={handleCreateProblem}>
      <Plus className="h-4 w-4 mr-1" />
      {t('admin.createProblem')}
    </Button>
  );

  return (
    <>
      {contestId ? (
        <DataTable
          columns={contestColumns}
          queryKey={problemsQueryKey}
          fetchFn={(api, params) =>
            fetchContestProblems(api, { ...params, contestId })
          }
          searchable
          searchPlaceholder={t('problems.searchPlaceholder')}
          defaultPerPage={20}
          emptyMessage={t('admin.noProblems')}
          state={table.state}
          onPageChange={table.setPage}
          onSearchChange={table.setSearch}
          onSortChange={table.setSort}
          toolbar={createButton}
        />
      ) : (
        <DataTable
          columns={columns}
          queryKey={problemsQueryKey}
          fetchFn={fetchProblems}
          searchable
          searchPlaceholder={t('problems.searchPlaceholder')}
          defaultPerPage={20}
          defaultSortBy="created_at"
          defaultSortOrder="desc"
          emptyMessage={t('admin.noProblems')}
          state={table.state}
          onPageChange={table.setPage}
          onSearchChange={table.setSearch}
          onSortChange={table.setSort}
          toolbar={createButton}
        />
      )}
      <ProblemFormDialog
        problem={editingProblem}
        contestId={contestId}
        open={problemDialogOpen}
        onOpenChange={setProblemDialogOpen}
      />
      {managingProblem && (
        <TestCasesDialog
          problem={managingProblem}
          open={testCasesDialogOpen}
          onOpenChange={setTestCasesDialogOpen}
        />
      )}
      {additionalFilesProblem && (
        <AdditionalFilesDialog
          problem={additionalFilesProblem}
          open={additionalFilesDialogOpen}
          onOpenChange={setAdditionalFilesDialogOpen}
        />
      )}
      {attachmentsProblem && (
        <AttachmentsDialog
          problem={attachmentsProblem}
          open={attachmentsDialogOpen}
          onOpenChange={setAttachmentsDialogOpen}
        />
      )}
      {checkerSourceProblem && (
        <CheckerSourceDialog
          problem={checkerSourceProblem}
          open={checkerSourceDialogOpen}
          onOpenChange={setCheckerSourceDialogOpen}
        />
      )}
      {configProblem && (
        <ResourceConfigDialog
          scope={{ scope: 'problem', problemId: configProblem.id }}
          resourceLabel={configProblem.title}
          open={configDialogOpen}
          onOpenChange={setConfigDialogOpen}
        />
      )}
      {contestId && configContestProblem && (
        <ResourceConfigDialog
          scope={{
            scope: 'contest_problem',
            contestId,
            problemId: configContestProblem.problem_id,
          }}
          resourceLabel={configContestProblem.problem_title}
          open={configDialogOpen}
          onOpenChange={setConfigDialogOpen}
        />
      )}
    </>
  );
}
