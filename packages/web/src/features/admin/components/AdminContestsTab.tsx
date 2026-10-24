import {
  type ApiClient,
  getErrorMessage,
  useApiClient,
} from '@broccoli/web-sdk/api';
import type { ContestProblem, ContestSummary } from '@broccoli/web-sdk/contest';
import {
  type ServerTableParams,
  useIdempotencyKey,
  useRegistries,
} from '@broccoli/web-sdk/hooks';
import { useTranslation } from '@broccoli/web-sdk/i18n';
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
} from '@broccoli/web-sdk/ui';
import { formatDateTime } from '@broccoli/web-sdk/utils';
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
import { useEffect, useState } from 'react';
import { Link } from 'react-router';
import { toast } from 'sonner';

import { ResourceConfigDialog } from '@/components/config';
import { Markdown } from '@/components/Markdown';
import { MarkdownEditor } from '@/components/MarkdownEditor';
import { ManageParticipantsDialog } from '@/features/admin/components/ManageParticipantsDialog';
import { SwitchField } from '@/features/admin/components/SwitchField';
import { ContestSlotProvider } from '@/features/contest/components/ContestSlotProvider';
import { getContestStatus } from '@/features/contest/utils/status';
import { useTableSearchParams } from '@/hooks/use-table-search-params';

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
  const apiClient = useApiClient();
  const { data, isLoading } = useQuery({
    queryKey: ['problem-preview', problemId],
    queryFn: async () => {
      const { data, error } = await apiClient.GET('/problems/{id}', {
        params: { path: { id: problemId } },
      });
      if (error) throw error;
      return data;
    },
    enabled: open,
  });

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-3xl max-h-[85vh] flex flex-col">
        <DialogHeader>
          <DialogTitle>
            {data ? `#${data.id} — ${data.title}` : 'Loading...'}
          </DialogTitle>
          {data && (
            <DialogDescription>
              {data.time_limit}ms · {(data.memory_limit / 1024).toFixed(0)}MB
            </DialogDescription>
          )}
        </DialogHeader>
        <div className="overflow-y-auto flex-1 pr-2">
          {isLoading ? (
            <div className="py-12 text-center text-muted-foreground">
              Loading...
            </div>
          ) : data ? (
            <Markdown>{data.content}</Markdown>
          ) : null}
        </div>
      </DialogContent>
    </Dialog>
  );
}

// -- Contest Form Dialog --

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
  const queryClient = useQueryClient();
  const isEdit = !!contest;

  const [title, setTitle] = useState('');
  const [description, setDescription] = useState('');
  const [activateTime, setActivateTime] = useState<Date | undefined>(undefined);
  const [startTime, setStartTime] = useState<Date | undefined>(undefined);
  const [endTime, setEndTime] = useState<Date | undefined>(undefined);
  const [deactivateTime, setDeactivateTime] = useState<Date | undefined>(
    undefined,
  );
  const [isPublic, setIsPublic] = useState(false);
  const [submissionsVisible, setSubmissionsVisible] = useState(false);
  const [showCompileOutput, setShowCompileOutput] = useState(true);
  const [showParticipantsList, setShowParticipantsList] = useState(true);
  const [contestType, setContestType] = useState('');
  const [loading, setLoading] = useState(false);
  const [loadingData, setLoadingData] = useState(false);
  const apiClient = useApiClient();
  const { getKey, resetKey } = useIdempotencyKey();
  const { data: registries } = useRegistries();

  useEffect(() => {
    if (!open) return;
    if (contest) {
      setLoadingData(true);
      apiClient
        .GET('/contests/{id}', { params: { path: { id: contest.id } } })
        .then(({ data, error }) => {
          setLoadingData(false);
          if (error || !data) return;
          setTitle(data.title);
          setDescription(data.description);
          setActivateTime(
            data.activate_time ? new Date(data.activate_time) : undefined,
          );
          setStartTime(new Date(data.start_time));
          setEndTime(new Date(data.end_time));
          setDeactivateTime(
            data.deactivate_time ? new Date(data.deactivate_time) : undefined,
          );
          setIsPublic(data.is_public);
          setSubmissionsVisible(data.submissions_visible);
          setShowCompileOutput(data.show_compile_output);
          setShowParticipantsList(data.show_participants_list);
          setContestType(data.contest_type ?? '');
        });
    } else {
      setTitle('');
      setDescription('');
      setActivateTime(undefined);
      setStartTime(undefined);
      setEndTime(undefined);
      setDeactivateTime(undefined);
      setIsPublic(false);
      setSubmissionsVisible(false);
      setShowCompileOutput(true);
      setShowParticipantsList(true);
      setContestType('');
    }
  }, [apiClient, open, contest]);

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();

    if (!title.trim()) {
      toast.error(t('validation.titleRequired'));
      return;
    }
    if (!startTime || !endTime) {
      toast.error(t('validation.startEndTimeRequired'));
      return;
    }
    if (startTime >= endTime) {
      toast.error(t('validation.startBeforeEnd'));
      return;
    }
    if (activateTime && activateTime > startTime) {
      toast.error(t('validation.activateBeforeStart'));
      return;
    }
    if (deactivateTime && deactivateTime < endTime) {
      toast.error(t('validation.deactivateAfterEnd'));
      return;
    }

    if (!contestType) {
      toast.error(t('validation.contestTypeRequiredForContest'));
      return;
    }

    setLoading(true);

    const body = {
      title,
      description,
      activate_time: activateTime?.toISOString() ?? null,
      start_time: startTime.toISOString(),
      end_time: endTime.toISOString(),
      deactivate_time: deactivateTime?.toISOString() ?? null,
      is_public: isPublic,
      submissions_visible: submissionsVisible,
      show_compile_output: showCompileOutput,
      show_participants_list: showParticipantsList,
      contest_type: contestType,
    };

    const result = isEdit
      ? await apiClient.PATCH('/contests/{id}', {
          params: { path: { id: contest!.id } },
          body,
        })
      : await apiClient.POST('/contests', {
          headers: { 'Idempotency-Key': getKey() },
          body,
        });

    setLoading(false);
    if (result.error) {
      toast.error(
        getErrorMessage(
          result.error,
          isEdit ? t('admin.editError') : t('admin.createError'),
        ),
      );
    } else {
      if (!isEdit) resetKey();
      toast.success(
        isEdit ? t('toast.contest.updated') : t('toast.contest.created'),
      );
      queryClient.invalidateQueries({ queryKey: ['admin-contests'] });
      onOpenChange(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-2xl max-h-[90vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>
            {isEdit ? t('admin.editContest') : t('admin.createContest')}
          </DialogTitle>
          <DialogDescription>
            {isEdit ? '' : t('admin.createContestDesc')}
          </DialogDescription>
        </DialogHeader>

        {loadingData ? (
          <div className="py-8 text-center text-muted-foreground">
            Loading...
          </div>
        ) : (
          <form onSubmit={handleSubmit} className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="contest-title">{t('admin.field.title')}</Label>
              <Input
                id="contest-title"
                value={title}
                onChange={(e) => setTitle(e.target.value)}
                required
                maxLength={256}
                placeholder="Weekly Contest #42"
              />
            </div>

            <div className="space-y-2">
              <Label htmlFor="contest-description">
                {t('admin.field.description')}
              </Label>
              <MarkdownEditor
                id="contest-description"
                value={description}
                onChange={setDescription}
                minHeight={150}
                placeholder="Contest description (Markdown supported)"
              />
            </div>

            <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
              <div className="space-y-2">
                <Label>{t('admin.field.activateTime')}</Label>
                <DateTimePicker
                  value={activateTime}
                  onChange={setActivateTime}
                  placeholder={t('admin.field.activateTime')}
                />
              </div>
              <div className="space-y-2">
                <Label>{t('admin.field.deactivateTime')}</Label>
                <DateTimePicker
                  value={deactivateTime}
                  onChange={setDeactivateTime}
                  placeholder={t('admin.field.deactivateTime')}
                />
              </div>
              <div className="space-y-2">
                <Label>{t('admin.field.startTime')}</Label>
                <DateTimePicker
                  value={startTime}
                  onChange={setStartTime}
                  placeholder={t('admin.field.startTime')}
                />
              </div>
              <div className="space-y-2">
                <Label>{t('admin.field.endTime')}</Label>
                <DateTimePicker
                  value={endTime}
                  onChange={setEndTime}
                  placeholder={t('admin.field.endTime')}
                />
              </div>
            </div>

            <div className="space-y-2">
              <Label htmlFor="contest-type">
                {t('admin.field.contestType')}
              </Label>
              <select
                id="contest-type"
                value={contestType}
                onChange={(e) => setContestType(e.target.value)}
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
                  checked={isPublic}
                  onCheckedChange={setIsPublic}
                />
                <SwitchField
                  id="contest-submissions"
                  label={t('admin.field.submissionsVisible')}
                  checked={submissionsVisible}
                  onCheckedChange={setSubmissionsVisible}
                />
                <SwitchField
                  id="contest-compile"
                  label={t('admin.field.showCompileOutput')}
                  checked={showCompileOutput}
                  onCheckedChange={setShowCompileOutput}
                />
                <SwitchField
                  id="contest-participants"
                  label={t('admin.field.showParticipantsList')}
                  checked={showParticipantsList}
                  onCheckedChange={setShowParticipantsList}
                />
              </div>
            </div>

            <DialogFooter>
              <Button type="submit" disabled={loading}>
                {loading
                  ? t('admin.saving')
                  : isEdit
                    ? t('admin.edit')
                    : t('admin.createContest')}
              </Button>
            </DialogFooter>
          </form>
        )}
      </DialogContent>
    </Dialog>
  );
}

// -- Contest Problems Dialog --

function nextLabel(usedLabels: Set<string>): string {
  // A-Z, then AA-AZ, BA-BZ, ..., ZZ (max 702)
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
  const { getKey, resetKey } = useIdempotencyKey();

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
    const autoLabel = nextLabel(usedLabels);
    if (!autoLabel) return;
    setAddingId(problemId);
    setErrorMsg('');
    const { error: apiError } = await apiClient.POST(
      '/contests/{id}/problems',
      {
        headers: { 'Idempotency-Key': getKey() },
        params: { path: { id: contest.id } },
        body: { problem_id: problemId, label: autoLabel },
      },
    );
    setAddingId(null);
    if (apiError) {
      toast.error(getErrorMessage(apiError, t('toast.problem.addError')));
    } else {
      resetKey();
      toast.success(t('toast.problem.added'));
      queryClient.invalidateQueries({ queryKey: contestProblemsKey });
    }
  }

  async function handleRemove(problemId: number) {
    if (!window.confirm(t('admin.deleteConfirm'))) return;
    const { error: apiError } = await apiClient.DELETE(
      '/contests/{id}/problems/{problem_id}',
      {
        params: { path: { id: contest.id, problem_id: problemId } },
      },
    );
    if (apiError) {
      toast.error(getErrorMessage(apiError, t('toast.problem.removeError')));
    } else {
      toast.success(t('toast.problem.removed'));
      queryClient.invalidateQueries({ queryKey: contestProblemsKey });
    }
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
                          onClick={() => handleRemove(p.problem_id)}
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
                Loading...
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
              )?.problem_title ?? `Problem ${configProblemId}`
            }
            open={configCPOpen}
            onOpenChange={setConfigCPOpen}
          />
        )}
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
  const { t, locale } = useTranslation();
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
          {formatDateTime(row.original.start_time, locale)}
        </span>
      ),
    },
    {
      accessorKey: 'end_time',
      header: t('contests.endTime'),
      size: 180,
      cell: ({ row }) => (
        <span className="text-muted-foreground">
          {formatDateTime(row.original.end_time, locale)}
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

// -- Contests Tab --

export function AdminContestsTab() {
  const { t } = useTranslation();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();
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

  async function handleDeleteContest(contest: ContestSummary) {
    if (!window.confirm(t('admin.deleteConfirm'))) return;
    const { error } = await apiClient.DELETE('/contests/{id}', {
      params: { path: { id: contest.id } },
    });
    if (error) {
      toast.error(getErrorMessage(error, t('toast.contest.deleteError')));
    } else {
      toast.success(t('toast.contest.deleted'));
      queryClient.invalidateQueries({ queryKey: ['admin-contests'] });
    }
  }

  const columns = useContestColumns({
    onEdit: handleEditContest,
    onDelete: handleDeleteContest,
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
    </>
  );
}
