import {
  type ApiClient,
  getErrorMessage,
  useApiClient,
} from '@broccoli/web-sdk/api';
import type { ContestProblem, ContestSummary } from '@broccoli/web-sdk/contest';
import {
  isAlreadyDone,
  type ServerTableParams,
  useAsyncDialog,
  useIdempotentRequest,
  useRegistries,
} from '@broccoli/web-sdk/hooks';
import { useFormat, useTranslation } from '@broccoli/web-sdk/i18n';
import type { ProblemSummary } from '@broccoli/web-sdk/problem';
import { Slot } from '@broccoli/web-sdk/slot';
import {
  Badge,
  Button,
  DataTable,
  type DataTableColumn,
  DateTimePicker,
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
  Input,
  Label,
  Separator,
  useConfirmDialog,
} from '@broccoli/web-sdk/ui';
import { formatKibibytes } from '@broccoli/web-sdk/utils';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import {
  Check,
  Eye,
  EyeOff,
  List,
  MoreHorizontal,
  Pencil,
  Plus,
  Search,
  Settings,
  Trash2,
  Users,
} from 'lucide-react';
import { type ReactNode, useEffect, useState } from 'react';
import { Link } from 'react-router';
import { toast } from 'sonner';

import { ResourceConfigDialog } from '@/components/config';
import { Markdown } from '@/components/Markdown';
import { MarkdownEditor } from '@/components/MarkdownEditor';
import { RecordLoader } from '@/components/RecordLoader';
import { ManageParticipantsDialog } from '@/features/admin/components/ManageParticipantsDialog';
import { SwitchField } from '@/features/admin/components/SwitchField';
import { ContestSlotProvider } from '@/features/contest/components/ContestSlotProvider';
import { nextProblemLabel } from '@/features/contest/utils/problem-label';
import { getContestStatus } from '@/features/contest/utils/status';
import { useTableSearchParams } from '@/hooks/use-table-search-params';
import { changedFields } from '@/lib/changed-fields';
import type { MessageKey } from '@/lib/i18n/catalog';
import { toastUnsettled } from '@/lib/idempotency-feedback';

// -- Data fetcher --

async function fetchContests(apiClient: ApiClient, params: ServerTableParams) {
  const { data, error } = await apiClient.GET('/contests', {
    params: {
      query: {
        page: params.page,
        per_page: params.per_page,
        search: params.search,
        sort_by: params.sort_by,
        sort_order: params.sort_order,
      },
    },
  });
  if (error) throw error;
  return { data: data.data, pagination: data.pagination };
}

// -- Problem Preview Dialog --

function ProblemPreviewDialog({
  problemId,
  open,
  onOpenChange,
}: {
  problemId: number;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const { t } = useTranslation();
  const apiClient = useApiClient();

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-3xl max-h-[85vh] flex flex-col">
        <RecordLoader
          key={problemId}
          allowCached
          queryKey={['problem-preview', problemId]}
          queryFn={async () => {
            const { data, error } = await apiClient.GET('/problems/{id}', {
              params: { path: { id: problemId } },
            });
            if (error) throw error;
            return data;
          }}
        >
          {(problem) => (
            <>
              <DialogHeader>
                <DialogTitle>
                  {t('admin.problemPreview.title', {
                    id: problem.id,
                    title: problem.title,
                  })}
                </DialogTitle>
                <DialogDescription>
                  {t('admin.problemPreview.limits', {
                    time: problem.time_limit,
                    memory: formatKibibytes(problem.memory_limit),
                  })}
                </DialogDescription>
              </DialogHeader>
              <div className="overflow-y-auto flex-1 pr-2">
                <Markdown>{problem.content}</Markdown>
              </div>
            </>
          )}
        </RecordLoader>
      </DialogContent>
    </Dialog>
  );
}

// -- Contest Form Dialog --

interface ContestFormValues {
  title: string;
  description: string;
  activateTime?: Date;
  startTime?: Date;
  endTime?: Date;
  deactivateTime?: Date;
  isPublic: boolean;
  submissionsVisible: boolean;
  showCompileOutput: boolean;
  showParticipantsList: boolean;
  contestType: string;
}

const NEW_CONTEST: ContestFormValues = {
  title: '',
  description: '',
  isPublic: false,
  submissionsVisible: false,
  showCompileOutput: true,
  showParticipantsList: true,
  contestType: '',
};

function toDate(value: string | null | undefined): Date | undefined {
  return value ? new Date(value) : undefined;
}

async function fetchContest(apiClient: ApiClient, id: number) {
  const { data, error } = await apiClient.GET('/contests/{id}', {
    params: { path: { id } },
  });
  if (error) throw error;
  return data;
}

function contestFormValues(
  contest: Awaited<ReturnType<typeof fetchContest>>,
): ContestFormValues {
  return {
    title: contest.title,
    description: contest.description,
    activateTime: toDate(contest.activate_time),
    startTime: toDate(contest.start_time),
    endTime: toDate(contest.end_time),
    deactivateTime: toDate(contest.deactivate_time),
    isPublic: contest.is_public,
    submissionsVisible: contest.submissions_visible,
    showCompileOutput: contest.show_compile_output,
    showParticipantsList: contest.show_participants_list,
    contestType: contest.contest_type ?? '',
  };
}

/** The create and update request body for validated `values`. */
function contestRequestBody(values: ContestFormValues) {
  return {
    title: values.title,
    description: values.description,
    activate_time: values.activateTime?.toISOString() ?? null,
    start_time: values.startTime!.toISOString(),
    end_time: values.endTime!.toISOString(),
    deactivate_time: values.deactivateTime?.toISOString() ?? null,
    is_public: values.isPublic,
    submissions_visible: values.submissionsVisible,
    show_compile_output: values.showCompileOutput,
    show_participants_list: values.showParticipantsList,
    contest_type: values.contestType,
  };
}

/** The i18n key of the first problem with `values`, or null when valid. */
function contestFormProblem(values: ContestFormValues): MessageKey | null {
  const { title, startTime, endTime, activateTime, deactivateTime } = values;
  if (!title.trim()) return 'validation.titleRequired';
  if (!startTime || !endTime) return 'validation.startEndTimeRequired';
  if (startTime >= endTime) return 'validation.startBeforeEnd';
  if (activateTime && activateTime > startTime) {
    return 'validation.activateBeforeStart';
  }
  if (deactivateTime && deactivateTime < endTime) {
    return 'validation.deactivateAfterEnd';
  }
  if (!values.contestType) return 'validation.contestTypeRequiredForContest';
  return null;
}

export function ContestFormDialog({
  contest,
  open,
  onOpenChange,
}: {
  contest?: ContestSummary;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const { t } = useTranslation();
  const apiClient = useApiClient();
  const dialog = useAsyncDialog({ open, onOpenChange });

  return (
    <Dialog open={dialog.open} onOpenChange={dialog.onOpenChange}>
      <DialogContent className="max-w-2xl max-h-[90vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>
            {contest ? t('admin.editContest') : t('admin.createContest')}
          </DialogTitle>
          <DialogDescription>
            {contest ? contest.title : t('admin.createContestDesc')}
          </DialogDescription>
        </DialogHeader>

        {contest ? (
          <RecordLoader
            key={contest.id}
            queryKey={['contest-edit', contest.id]}
            queryFn={() => fetchContest(apiClient, contest.id)}
          >
            {(record) => (
              <ContestForm
                contestId={contest.id}
                initial={contestFormValues(record)}
                dialog={dialog}
              />
            )}
          </RecordLoader>
        ) : (
          <ContestForm initial={NEW_CONTEST} dialog={dialog} />
        )}
      </DialogContent>
    </Dialog>
  );
}

function ContestForm({
  contestId,
  initial,
  dialog,
}: {
  /** The contest being edited; creates a new one when absent. */
  contestId?: number;
  initial: ContestFormValues;
  dialog: ReturnType<typeof useAsyncDialog>;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const apiClient = useApiClient();
  const sendIdempotent = useIdempotentRequest();
  const { data: registries } = useRegistries();
  const [values, setValues] = useState(initial);
  const isEdit = contestId !== undefined;

  const set =
    <K extends keyof ContestFormValues>(field: K) =>
    (value: ContestFormValues[K]) =>
      setValues((prev) => ({ ...prev, [field]: value }));

  async function save() {
    const body = contestRequestBody(values);

    const result = isEdit
      ? await apiClient.PATCH('/contests/{id}', {
          params: { path: { id: contestId } },
          body: changedFields(contestRequestBody(initial), body),
        })
      : await sendIdempotent(body, (key) =>
          apiClient.POST('/contests', {
            headers: { 'Idempotency-Key': key },
            body,
          }),
        );

    const refetch = () =>
      queryClient.invalidateQueries({ queryKey: ['admin-contests'] });
    // Unsettled: the form stays open, so if the contest is not in the list
    // it can be sent again as it is.
    if (toastUnsettled(result.error, t, { refetch })) throw result.error;
    if (result.error && !isAlreadyDone(result.error)) {
      toast.error(
        getErrorMessage(
          result.error,
          isEdit ? t('admin.editError') : t('admin.createError'),
        ),
      );
      throw result.error;
    }
    toast.success(
      isEdit ? t('toast.contest.updated') : t('toast.contest.created'),
    );
    refetch();
  }

  function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    const problem = contestFormProblem(values);
    if (problem) {
      toast.error(t(problem));
      return;
    }
    void dialog.submit(save);
  }

  return (
    <form onSubmit={handleSubmit} className="space-y-4">
      <fieldset disabled={dialog.submitting} className="space-y-4">
        <div className="space-y-2">
          <Label htmlFor="contest-title">{t('admin.field.title')}</Label>
          <Input
            id="contest-title"
            value={values.title}
            onChange={(e) => set('title')(e.target.value)}
            required
            maxLength={256}
            placeholder={t('admin.contest.titlePlaceholder')}
          />
        </div>

        <div className="space-y-2">
          <Label htmlFor="contest-description">
            {t('admin.field.description')}
          </Label>
          <MarkdownEditor
            id="contest-description"
            value={values.description}
            onChange={set('description')}
            minHeight={150}
            placeholder={t('admin.contest.descriptionPlaceholder')}
          />
        </div>

        <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
          <div className="space-y-2">
            <Label>{t('admin.field.activateTime')}</Label>
            <DateTimePicker
              value={values.activateTime}
              onChange={set('activateTime')}
              placeholder={t('admin.field.activateTime')}
            />
          </div>
          <div className="space-y-2">
            <Label>{t('admin.field.deactivateTime')}</Label>
            <DateTimePicker
              value={values.deactivateTime}
              onChange={set('deactivateTime')}
              placeholder={t('admin.field.deactivateTime')}
            />
          </div>
          <div className="space-y-2">
            <Label>{t('admin.field.startTime')}</Label>
            <DateTimePicker
              value={values.startTime}
              onChange={set('startTime')}
              placeholder={t('admin.field.startTime')}
            />
          </div>
          <div className="space-y-2">
            <Label>{t('admin.field.endTime')}</Label>
            <DateTimePicker
              value={values.endTime}
              onChange={set('endTime')}
              placeholder={t('admin.field.endTime')}
            />
          </div>
        </div>

        <div className="space-y-2">
          <Label htmlFor="contest-type">{t('admin.field.contestType')}</Label>
          <select
            id="contest-type"
            value={values.contestType}
            onChange={(e) => set('contestType')(e.target.value)}
            className="flex h-9 w-full rounded-md border border-input bg-transparent px-3 py-1 text-sm shadow-xs transition-colors focus-visible:outline-hidden focus-visible:ring-1 focus-visible:ring-ring"
          >
            <option value="" disabled>
              {t('admin.field.selectPlaceholder')}
            </option>
            {(registries?.contest_types ?? []).map((opt) => (
              <option key={opt} value={opt}>
                {opt}
              </option>
            ))}
          </select>
        </div>

        <Separator />

        <div className="space-y-3">
          <Label className="text-sm text-muted-foreground">
            {t('admin.field.options')}
          </Label>
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
            <SwitchField
              id="contest-public"
              label={t('admin.field.isPublic')}
              checked={values.isPublic}
              onCheckedChange={set('isPublic')}
            />
            <SwitchField
              id="contest-submissions"
              label={t('admin.field.submissionsVisible')}
              checked={values.submissionsVisible}
              onCheckedChange={set('submissionsVisible')}
            />
            <SwitchField
              id="contest-compile"
              label={t('admin.field.showCompileOutput')}
              checked={values.showCompileOutput}
              onCheckedChange={set('showCompileOutput')}
            />
            <SwitchField
              id="contest-participants"
              label={t('admin.field.showParticipantsList')}
              checked={values.showParticipantsList}
              onCheckedChange={set('showParticipantsList')}
            />
          </div>
        </div>
      </fieldset>

      <DialogFooter>
        <Button type="submit" disabled={dialog.submitting}>
          {dialog.submitting
            ? t('admin.saving')
            : isEdit
              ? t('admin.save')
              : t('admin.createContest')}
        </Button>
      </DialogFooter>
    </form>
  );
}

// -- Contest Problems Dialog --

export function ContestProblemsDialog({
  contest,
  open,
  onOpenChange,
}: {
  contest: ContestSummary;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const contestProblemsKey = ['contest-problems', contest.id];
  const apiClient = useApiClient();
  const sendIdempotent = useIdempotentRequest();
  const [confirm, confirmDialog] = useConfirmDialog();

  const { data: contestProblems = [], isLoading: loadingContestProblems } =
    useQuery({
      queryKey: contestProblemsKey,
      queryFn: async () => {
        const { data, error } = await apiClient.GET('/contests/{id}/problems', {
          params: { path: { id: contest.id } },
        });
        if (error) throw error;
        return data;
      },
      enabled: open,
    });

  const { data: allProblems = [], isLoading: loadingAllProblems } = useQuery({
    queryKey: ['all-problems-for-contest', contest.id],
    queryFn: async () => {
      const { data, error } = await apiClient.GET('/problems', {
        params: { query: { page: 1, per_page: 200 } },
      });
      if (error) throw error;
      return data.data;
    },
    enabled: open,
  });

  const [search, setSearch] = useState('');
  const [addingId, setAddingId] = useState<number | null>(null);
  const [errorMsg, setErrorMsg] = useState('');
  const [previewProblemId, setPreviewProblemId] = useState<number | null>(null);
  const [configCPOpen, setConfigCPOpen] = useState(false);
  const [configProblemId, setConfigProblemId] = useState<number | null>(null);

  useEffect(() => {
    if (open) {
      setSearch('');
      setErrorMsg('');
      setPreviewProblemId(null);
      setConfigCPOpen(false);
      setConfigProblemId(null);
    }
  }, [open]);

  const addedProblemIds = new Set(
    contestProblems.map((p: ContestProblem) => p.problem_id),
  );
  const usedLabels = new Set(
    contestProblems.map((p: ContestProblem) => p.label),
  );

  const filteredProblems = allProblems
    .filter(
      (p: ProblemSummary) =>
        !search ||
        p.title.toLowerCase().includes(search.toLowerCase()) ||
        String(p.id).includes(search),
    )
    .sort((a: ProblemSummary, b: ProblemSummary) => {
      const aAdded = addedProblemIds.has(a.id) ? 1 : 0;
      const bAdded = addedProblemIds.has(b.id) ? 1 : 0;
      if (aAdded !== bAdded) return aAdded - bAdded;
      return a.id - b.id;
    });

  async function handleAdd(problemId: number) {
    const autoLabel = nextProblemLabel(usedLabels);
    if (!autoLabel) return;
    setAddingId(problemId);
    setErrorMsg('');
    const body = { problem_id: problemId, label: autoLabel };
    const { error: apiError } = await sendIdempotent(
      { contestId: contest.id, body },
      (key) =>
        apiClient.POST('/contests/{id}/problems', {
          headers: { 'Idempotency-Key': key },
          params: { path: { id: contest.id } },
          body,
        }),
    );
    setAddingId(null);
    if (
      toastUnsettled(apiError, t, {
        refetch: () =>
          queryClient.invalidateQueries({ queryKey: contestProblemsKey }),
      })
    ) {
      return;
    }
    if (apiError && !isAlreadyDone(apiError)) {
      toast.error(getErrorMessage(apiError, t('toast.problem.addError')));
    } else {
      toast.success(t('toast.problem.added'));
      queryClient.invalidateQueries({ queryKey: contestProblemsKey });
    }
  }

  function handleRemove(problem: ContestProblem) {
    confirm({
      title: t('admin.removeFromContestConfirm'),
      description: problem.problem_title,
      confirmLabel: t('admin.removeFromContest'),
      destructive: true,
      failureMessage: t('toast.problem.removeError'),
      onConfirm: async () => {
        const { error } = await apiClient.DELETE(
          '/contests/{id}/problems/{problem_id}',
          {
            params: {
              path: { id: contest.id, problem_id: problem.problem_id },
            },
          },
        );
        if (error) throw error;
        toast.success(t('toast.problem.removed'));
        queryClient.invalidateQueries({ queryKey: contestProblemsKey });
      },
    });
  }

  const isLoading = loadingContestProblems || loadingAllProblems;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-2xl max-h-[90vh] flex flex-col">
        <DialogHeader>
          <DialogTitle>{t('admin.contestProblems')}</DialogTitle>
          <DialogDescription>{contest.title}</DialogDescription>
        </DialogHeader>

        {!isLoading && contestProblems.length > 0 && (
          <div className="rounded-md border overflow-y-auto max-h-60">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b bg-muted/40">
                  <th className="px-3 py-2 text-left font-medium text-foreground/80 w-16">
                    {t('admin.field.label')}
                  </th>
                  <th className="px-3 py-2 text-left font-medium text-foreground/80">
                    {t('admin.field.title')}
                  </th>
                  <th className="px-3 py-2 text-right font-medium text-foreground/80 w-20" />
                </tr>
              </thead>
              <tbody>
                {contestProblems.map((p: ContestProblem) => (
                  <tr
                    key={p.problem_id}
                    className="border-b last:border-0 hover:bg-muted/30"
                  >
                    <td className="px-3 py-2 font-medium">{p.label}</td>
                    <td className="px-3 py-2">{p.problem_title}</td>
                    <td className="px-3 py-2 text-right">
                      <div className="flex items-center justify-end gap-1">
                        <Button
                          variant="ghost"
                          size="icon"
                          className="h-7 w-7"
                          onClick={() => setPreviewProblemId(p.problem_id)}
                        >
                          <Eye className="h-3.5 w-3.5" />
                        </Button>
                        <Button
                          variant="ghost"
                          size="icon"
                          className="h-7 w-7"
                          onClick={() => {
                            setConfigProblemId(p.problem_id);
                            setConfigCPOpen(true);
                          }}
                        >
                          <Settings className="h-3.5 w-3.5" />
                        </Button>
                        <Button
                          variant="ghost"
                          size="icon"
                          className="h-7 w-7 text-destructive hover:text-destructive"
                          onClick={() => handleRemove(p)}
                        >
                          <Trash2 className="h-3.5 w-3.5" />
                        </Button>
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}

        <Separator />

        <div className="space-y-2 min-h-0 flex flex-col">
          <Label className="text-sm font-medium">
            {t('admin.availableProblems')}
          </Label>
          <div className="relative">
            <Search
              className="pointer-events-none absolute top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground"
              style={{ insetInlineStart: '0.625rem' }}
            />
            <Input
              placeholder={t('problems.searchPlaceholder')}
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              className="h-8 text-sm"
              style={{ paddingInlineStart: '2rem' }}
            />
          </div>

          {errorMsg && <p className="text-sm text-destructive">{errorMsg}</p>}

          <div className="overflow-y-auto max-h-64 rounded-md border">
            {isLoading ? (
              <div className="py-8 text-center text-muted-foreground">
                {t('common.loading')}
              </div>
            ) : filteredProblems.length === 0 ? (
              <div className="py-8 text-center text-sm text-muted-foreground">
                {t('problems.empty')}
              </div>
            ) : (
              <table className="w-full text-sm">
                <tbody>
                  {filteredProblems.map((p: ProblemSummary) => {
                    const isAdded = addedProblemIds.has(p.id);
                    const contestProblem = contestProblems.find(
                      (cp: ContestProblem) => cp.problem_id === p.id,
                    );
                    return (
                      <tr
                        key={p.id}
                        className={`border-b last:border-0 ${isAdded ? 'opacity-50' : 'hover:bg-muted/30'}`}
                      >
                        <td className="px-3 py-2 text-muted-foreground w-12">
                          #{p.id}
                        </td>
                        <td className="px-3 py-2">
                          <button
                            type="button"
                            className={`text-left hover:underline ${isAdded ? 'text-muted-foreground' : 'font-medium'}`}
                            onClick={() => setPreviewProblemId(p.id)}
                          >
                            {p.title}
                          </button>
                        </td>
                        <td className="px-3 py-2 text-right w-24">
                          {isAdded ? (
                            <Badge variant="secondary" className="text-xs">
                              <Check className="h-3 w-3 mr-1" />
                              {contestProblem?.label}
                            </Badge>
                          ) : (
                            <Button
                              variant="outline"
                              size="sm"
                              className="h-7 text-xs"
                              disabled={addingId === p.id}
                              onClick={() => handleAdd(p.id)}
                            >
                              <Plus className="h-3 w-3 mr-1" />
                              {addingId === p.id
                                ? t('admin.adding')
                                : t('admin.addProblem')}
                            </Button>
                          )}
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            )}
          </div>
        </div>

        {previewProblemId !== null && (
          <ProblemPreviewDialog
            problemId={previewProblemId}
            open={previewProblemId !== null}
            onOpenChange={(v) => {
              if (!v) setPreviewProblemId(null);
            }}
          />
        )}
        {configProblemId !== null && (
          <ResourceConfigDialog
            scope={{
              scope: 'contest_problem',
              contestId: contest.id,
              problemId: configProblemId,
            }}
            resourceLabel={
              contestProblems.find(
                (p: ContestProblem) => p.problem_id === configProblemId,
              )?.problem_title ??
              t('admin.problemFallbackLabel', { id: configProblemId })
            }
            open={configCPOpen}
            onOpenChange={setConfigCPOpen}
          />
        )}
        {confirmDialog}
      </DialogContent>
    </Dialog>
  );
}

// -- Column hook --

function useContestColumns({
  onEdit,
  onDelete,
  onManageProblems,
  onBulkParticipants,
  onConfigure,
}: {
  onEdit: (contest: ContestSummary) => void;
  onDelete: (contest: ContestSummary) => void;
  onManageProblems: (contest: ContestSummary) => void;
  onBulkParticipants: (contest: ContestSummary) => void;
  onConfigure: (contest: ContestSummary) => void;
}): DataTableColumn<ContestSummary>[] {
  const { t } = useTranslation();
  const format = useFormat();
  return [
    { accessorKey: 'id', header: '#', size: 60 },
    {
      accessorKey: 'title',
      header: t('admin.field.title'),
      sortKey: 'title',
      cell: ({ row }) => (
        <Link
          to={`/contests/${row.original.id}`}
          className="font-medium hover:text-primary hover:underline"
        >
          {row.original.title}
        </Link>
      ),
    },
    {
      id: 'status',
      header: t('contests.status'),
      size: 110,
      cell: ({ row }) => {
        const { label, variant } = getContestStatus(
          row.original.start_time,
          row.original.end_time,
          t,
        );
        return <Badge variant={variant}>{label}</Badge>;
      },
    },
    {
      id: 'contest_type',
      header: t('admin.field.contestType'),
      size: 120,
      cell: ({ row }) =>
        row.original.contest_type ? (
          <Badge variant="outline">{row.original.contest_type}</Badge>
        ) : (
          <span className="text-destructive">
            {t('admin.field.contestTypeMissing')}
          </span>
        ),
    },
    {
      id: 'visibility',
      header: '',
      size: 40,
      cell: ({ row }) =>
        row.original.is_public ? (
          <Eye className="h-3.5 w-3.5 text-muted-foreground" />
        ) : (
          <EyeOff className="h-3.5 w-3.5 text-muted-foreground" />
        ),
    },
    {
      accessorKey: 'start_time',
      header: t('contests.startTime'),
      size: 180,
      sortKey: 'start_time',
      cell: ({ row }) => (
        <span className="text-muted-foreground">
          {format.dateTime(row.original.start_time, { timeZoneName: true })}
        </span>
      ),
    },
    {
      accessorKey: 'end_time',
      header: t('contests.endTime'),
      size: 180,
      cell: ({ row }) => (
        <span className="text-muted-foreground">
          {format.dateTime(row.original.end_time, { timeZoneName: true })}
        </span>
      ),
    },
    {
      id: 'actions',
      header: '',
      size: 50,
      cell: ({ row }) => (
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button variant="ghost" size="icon" className="h-7 w-7">
              <MoreHorizontal className="h-4 w-4" />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            <DropdownMenuItem onClick={() => onManageProblems(row.original)}>
              <List className="h-4 w-4" />
              {t('admin.contestProblems')}
            </DropdownMenuItem>
            <DropdownMenuItem onClick={() => onBulkParticipants(row.original)}>
              <Users className="h-4 w-4" />
              {t('admin.bulkParticipantsAction')}
            </DropdownMenuItem>
            <DropdownMenuItem onClick={() => onConfigure(row.original)}>
              <Settings className="h-4 w-4" />
              {t('admin.configure')}
            </DropdownMenuItem>
            <ContestSlotProvider contestId={row.original.id}>
              <Slot name="contest.admin.menu" slotProps={{ variant: 'menu' }} />
            </ContestSlotProvider>
            <DropdownMenuItem onClick={() => onEdit(row.original)}>
              <Pencil className="h-4 w-4" />
              {t('admin.edit')}
            </DropdownMenuItem>
            <DropdownMenuSeparator />
            <DropdownMenuItem
              className="text-destructive focus:text-destructive"
              onClick={() => onDelete(row.original)}
            >
              <Trash2 className="h-4 w-4" />
              {t('admin.delete')}
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      ),
    },
  ];
}

// -- Contest deletion --

/**
 * Ask to delete a contest and delete it once confirmed. Returns the function
 * that asks and the confirm dialog to render.
 */
export function useDeleteContestConfirm(): [
  (contest: { id: number; title: string }, onDeleted?: () => void) => void,
  ReactNode,
] {
  const { t } = useTranslation();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();
  const [confirm, confirmDialog] = useConfirmDialog();

  const confirmDelete = (
    contest: { id: number; title: string },
    onDeleted?: () => void,
  ) =>
    confirm({
      title: t('admin.deleteConfirm'),
      description: contest.title,
      confirmLabel: t('admin.delete'),
      destructive: true,
      failureMessage: t('toast.contest.deleteError'),
      onConfirm: async () => {
        const { error } = await apiClient.DELETE('/contests/{id}', {
          params: { path: { id: contest.id } },
        });
        if (error) throw error;
        toast.success(t('toast.contest.deleted'));
        queryClient.invalidateQueries({ queryKey: ['admin-contests'] });
        onDeleted?.();
      },
    });

  return [confirmDelete, confirmDialog];
}

// -- Contests Tab --

export function AdminContestsTab() {
  const { t } = useTranslation();
  const [confirmDelete, deleteDialog] = useDeleteContestConfirm();
  const table = useTableSearchParams({
    defaultSortBy: 'created_at',
    defaultSortOrder: 'desc',
  });

  const [contestDialogOpen, setContestDialogOpen] = useState(false);
  const [editingContest, setEditingContest] = useState<
    ContestSummary | undefined
  >();
  const [contestProblemsDialogOpen, setContestProblemsDialogOpen] =
    useState(false);
  const [managingContest, setManagingContest] = useState<
    ContestSummary | undefined
  >();
  const [participantsDialogOpen, setParticipantsDialogOpen] = useState(false);
  const [participantsContest, setParticipantsContest] = useState<
    ContestSummary | undefined
  >();
  const [configDialogOpen, setConfigDialogOpen] = useState(false);
  const [configContest, setConfigContest] = useState<
    ContestSummary | undefined
  >();

  function handleCreateContest() {
    setEditingContest(undefined);
    setContestDialogOpen(true);
  }

  function handleEditContest(contest: ContestSummary) {
    setEditingContest(contest);
    setContestDialogOpen(true);
  }

  function handleManageProblems(contest: ContestSummary) {
    setManagingContest(contest);
    setContestProblemsDialogOpen(true);
  }

  function handleBulkParticipants(contest: ContestSummary) {
    setParticipantsContest(contest);
    setParticipantsDialogOpen(true);
  }

  function handleConfigure(contest: ContestSummary) {
    setConfigContest(contest);
    setConfigDialogOpen(true);
  }

  const columns = useContestColumns({
    onEdit: handleEditContest,
    onDelete: (contest) => confirmDelete(contest),
    onManageProblems: handleManageProblems,
    onBulkParticipants: handleBulkParticipants,
    onConfigure: handleConfigure,
  });

  return (
    <>
      <DataTable
        columns={columns}
        queryKey={['admin-contests']}
        fetchFn={fetchContests}
        searchable
        searchPlaceholder={t('contests.searchPlaceholder')}
        defaultPerPage={20}
        defaultSortBy="created_at"
        defaultSortOrder="desc"
        emptyMessage={t('admin.noContests')}
        state={table.state}
        onPageChange={table.setPage}
        onSearchChange={table.setSearch}
        onSortChange={table.setSort}
        toolbar={
          <Button size="sm" onClick={handleCreateContest}>
            <Plus className="h-4 w-4 mr-1" />
            {t('admin.createContest')}
          </Button>
        }
      />
      <ContestFormDialog
        contest={editingContest}
        open={contestDialogOpen}
        onOpenChange={setContestDialogOpen}
      />
      {managingContest && (
        <ContestProblemsDialog
          contest={managingContest}
          open={contestProblemsDialogOpen}
          onOpenChange={setContestProblemsDialogOpen}
        />
      )}
      {participantsContest && (
        <ManageParticipantsDialog
          contest={participantsContest}
          open={participantsDialogOpen}
          onOpenChange={setParticipantsDialogOpen}
        />
      )}
      {configContest && (
        <ResourceConfigDialog
          scope={{ scope: 'contest', contestId: configContest.id }}
          resourceLabel={configContest.title}
          open={configDialogOpen}
          onOpenChange={setConfigDialogOpen}
        />
      )}
      {deleteDialog}
    </>
  );
}
