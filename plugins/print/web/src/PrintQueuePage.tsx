import { useAuth } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import {
  Button,
  DataTable,
  type DataTableColumn,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
  Textarea,
} from '@broccoli/web-sdk/ui';
import { useQuery } from '@tanstack/react-query';
import {
  AlertTriangle,
  Ban,
  CheckCircle2,
  Code2,
  Crosshair,
  Printer,
  RotateCcw,
} from 'lucide-react';
import { useCallback, useEffect, useMemo, useState } from 'react';

import { StationsPanel } from './components/StationsPanel';
import { StatusPill } from './components/StatusPill';
import { usePrintApi } from './hooks/usePrintApi';
import { ALL_STATUSES, formatRelative } from './lib/format';
import type { PrintJob } from './types';

const PER_PAGE = 25;

export function PrintQueuePage() {
  const { t } = useTranslation();
  const { user } = useAuth();
  const api = usePrintApi();

  const [statusFilter, setStatusFilter] = useState('');
  const [stationFilter, setStationFilter] = useState('');
  const [printerFilter, setPrinterFilter] = useState('');
  const [nonce, setNonce] = useState(0);
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [cancelTarget, setCancelTarget] = useState<PrintJob | null>(null);
  const [reprintTarget, setReprintTarget] = useState<PrintJob | null>(null);
  const [batchConfirm, setBatchConfirm] = useState<{
    type: 'reprint' | 'cancel' | 'pin';
    count: number;
    fn: (printer?: string | null) => Promise<void>;
  } | null>(null);
  const [pinPrinter, setPinPrinter] = useState<string>('');
  const [codeViewJob, setCodeViewJob] = useState<PrintJob | null>(null);
  const [codeSource, setCodeSource] = useState<string | null>(null);

  // Pull station/printer lists for the filter dropdowns.
  const { data: stationsResp } = useQuery({
    queryKey: ['print', 'stations'],
    queryFn: () => api.listStations(),
    refetchInterval: 15_000,
  });
  const stations = stationsResp?.data ?? [];
  const stationNames = stations.map((s) => s.name);
  const printerNames = [
    ...new Set(
      stations.flatMap((s) => (Array.isArray(s.printers) ? s.printers : [])),
    ),
  ];

  // Poll the jobs table every 10 seconds.
  useEffect(() => {
    const id = setInterval(() => setNonce((n) => n + 1), 10_000);
    return () => clearInterval(id);
  }, []);

  const refresh = useCallback(() => {
    setNonce((n) => n + 1);
    setSelected(new Set());
  }, []);

  const act = useCallback(
    async (fn: () => Promise<unknown>) => {
      try {
        await fn();
      } finally {
        refresh();
      }
    },
    [refresh],
  );

  const handleViewCode = useCallback(
    async (job: PrintJob) => {
      setCodeViewJob(job);
      setCodeSource(null);
      try {
        const r = await api.getJob(job.id);
        setCodeSource(r.data.source);
      } catch {
        setCodeSource('(failed to load)');
      }
    },
    [api],
  );

  const batchAct = useCallback(
    (action: (id: number) => Promise<unknown>) => {
      if (selected.size === 0) return Promise.resolve();
      return act(async () => {
        for (const id of selected) await action(id);
      });
    },
    [selected, act],
  );

  const toggleOne = useCallback((id: number, on: boolean) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (on) next.add(id);
      else next.delete(id);
      return next;
    });
  }, []);

  const fetchJobs = useCallback(
    (
      _api: unknown,
      params: {
        page: number;
        per_page: number;
        search?: string;
        sort_by?: string;
        sort_order?: 'asc' | 'desc';
      },
    ) =>
      api.adminListJobs({
        ...params,
        status: statusFilter || undefined,
        station: stationFilter || undefined,
        printer: printerFilter || undefined,
      }),
    [api, statusFilter, stationFilter, printerFilter],
  );

  const columns = useMemo<DataTableColumn<PrintJob>[]>(
    () => [
      {
        id: 'select',
        header: ({ table }) => {
          const pageIds = table
            .getRowModel()
            .rows.map((r) => (r.original as PrintJob).id);
          const allChecked =
            pageIds.length > 0 && pageIds.every((id) => selected.has(id));
          return (
            <input
              type="checkbox"
              className="h-4 w-4 cursor-pointer accent-primary"
              checked={allChecked}
              onChange={() => {
                setSelected((prev) => {
                  const next = new Set(prev);
                  if (allChecked) pageIds.forEach((id) => next.delete(id));
                  else pageIds.forEach((id) => next.add(id));
                  return next;
                });
              }}
            />
          );
        },
        cell: ({ row }) => (
          <input
            type="checkbox"
            className="h-4 w-4 cursor-pointer accent-primary"
            checked={selected.has(row.original.id)}
            onChange={(e) => toggleOne(row.original.id, e.target.checked)}
            onClick={(e) => e.stopPropagation()}
          />
        ),
      },
      {
        id: 'id',
        header: t('print.queue.col.id'),
        cell: ({ row }) => (
          <span className="font-mono text-xs text-muted-foreground">
            #{row.original.id}
          </span>
        ),
      },
      {
        id: 'who',
        header: t('print.queue.col.who'),
        cell: ({ row }) => (
          <span className="font-medium text-foreground">
            {row.original.display_name || row.original.username}
          </span>
        ),
      },
      {
        id: 'problem',
        header: t('print.queue.col.problem'),
        cell: ({ row }) =>
          row.original.problem_label ? (
            <span className="inline-flex h-6 min-w-6 items-center justify-center rounded-md border border-border bg-muted px-1.5 font-mono text-xs font-semibold text-foreground">
              {row.original.problem_label}
            </span>
          ) : (
            <span className="text-muted-foreground">—</span>
          ),
      },
      {
        id: 'language',
        header: t('print.queue.col.language'),
        cell: ({ row }) => (
          <span className="text-xs text-muted-foreground">
            {row.original.language}
          </span>
        ),
      },
      {
        id: 'file',
        header: t('print.queue.col.file'),
        cell: ({ row }) => (
          <code className="font-mono text-xs text-foreground">
            {row.original.filename}
          </code>
        ),
      },
      {
        id: 'pages',
        header: t('print.queue.col.pages'),
        cell: ({ row }) => (
          <span className="tabular-nums text-xs text-muted-foreground">
            {row.original.pages ?? row.original.pages_est ?? '—'}
          </span>
        ),
      },
      {
        id: 'status',
        header: t('print.queue.col.status'),
        cell: ({ row }) => <StatusPill status={row.original.status} />,
      },
      {
        id: 'station',
        header: t('print.queue.col.station'),
        cell: ({ row }) =>
          row.original.claimed_by ? (
            <span className="text-xs text-foreground">
              {row.original.claimed_by}
              {row.original.claimed_printer && (
                <span className="text-muted-foreground">
                  {' '}
                  · {row.original.claimed_printer}
                </span>
              )}
            </span>
          ) : (
            <span className="text-muted-foreground">—</span>
          ),
      },
      {
        id: 'created_at',
        accessorKey: 'created_at',
        sortKey: 'created_at',
        header: t('print.queue.col.created'),
        cell: ({ row }) => (
          <span className="whitespace-nowrap text-xs text-muted-foreground tabular-nums">
            {formatRelative(row.original.created_at)}
          </span>
        ),
      },
      {
        id: 'actions',
        header: '',
        cell: ({ row }) => {
          const job = row.original;
          return (
            <div className="flex items-center justify-end gap-1">
              <Button
                variant="ghost"
                size="sm"
                className="h-7 gap-1 text-muted-foreground hover:text-foreground"
                onClick={() => handleViewCode(job)}
                title={t('print.queue.action.viewCode')}
              >
                <Code2 className="h-4 w-4" />
              </Button>
              {job.status === 'pending_approval' && (
                <Button
                  variant="ghost"
                  size="sm"
                  className="h-7 gap-1 text-emerald-600 hover:text-emerald-700 dark:text-emerald-400"
                  onClick={() => act(() => api.approveJob(job.id))}
                  title={t('print.queue.action.approve')}
                >
                  <CheckCircle2 className="h-4 w-4" />
                </Button>
              )}
              {['done', 'failed', 'canceled'].includes(job.status) && (
                <Button
                  variant="ghost"
                  size="sm"
                  className="h-7 gap-1 text-muted-foreground hover:text-foreground"
                  onClick={() => setReprintTarget(job)}
                  title={t('print.queue.action.reprint')}
                >
                  <RotateCcw className="h-4 w-4" />
                </Button>
              )}
              {!['done', 'canceled'].includes(job.status) && (
                <Button
                  variant="ghost"
                  size="sm"
                  className="h-7 gap-1 text-muted-foreground hover:text-red-600 dark:hover:text-red-400"
                  onClick={() => setCancelTarget(job)}
                  title={t('print.queue.action.cancel')}
                >
                  <Ban className="h-4 w-4" />
                </Button>
              )}
              {printerNames.length > 0 &&
                !['done', 'canceled'].includes(job.status) && (
                  <Select
                    value=""
                    onValueChange={(v) =>
                      act(() =>
                        api.pinJob(job.id, v === '__clear__' ? null : v),
                      )
                    }
                  >
                    <SelectTrigger
                      className="h-7 w-7 px-0"
                      title={t('print.queue.action.pin')}
                    >
                      <Crosshair className="h-3.5 w-3.5" />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="__clear__">
                        {t('print.queue.action.clearPin')}
                      </SelectItem>
                      {printerNames.map((p) => (
                        <SelectItem key={p} value={p}>
                          {p}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                )}
            </div>
          );
        },
      },
    ],
    [t, api, act, selected, toggleOne, handleViewCode, printerNames],
  );

  const filterSelect = (
    label: string,
    value: string,
    onChange: (v: string) => void,
    options: { value: string; label: string }[],
  ) => (
    <Select
      value={value || 'all'}
      onValueChange={(v) => onChange(v === 'all' ? '' : v)}
    >
      <SelectTrigger className="h-7 w-32 text-xs">
        <SelectValue placeholder={label} />
      </SelectTrigger>
      <SelectContent>
        <SelectItem value="all">{label}</SelectItem>
        {options.map((o) => (
          <SelectItem key={o.value} value={o.value}>
            {o.label}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );

  const toolbar = (
    <div className="flex flex-wrap items-center gap-2">
      {filterSelect(
        t('print.queue.col.status'),
        statusFilter,
        setStatusFilter,
        ALL_STATUSES.map((s) => ({ value: s, label: t(`print.status.${s}`) })),
      )}
      {stationNames.length > 0 &&
        filterSelect(
          t('print.queue.col.station'),
          stationFilter,
          setStationFilter,
          stationNames.map((s) => ({ value: s, label: s })),
        )}
      {printerNames.length > 0 &&
        filterSelect(
          t('print.stations.col.printers'),
          printerFilter,
          setPrinterFilter,
          printerNames.map((p) => ({ value: p, label: p })),
        )}
      {selected.size > 0 && (
        <>
          <span className="w-px h-5 bg-border" />
          <span className="text-xs text-muted-foreground">
            {t('print.queue.selected', { count: selected.size })}
          </span>
          <Button
            variant="outline"
            size="sm"
            className="h-7 gap-1 text-xs"
            onClick={() =>
              setBatchConfirm({
                type: 'reprint',
                count: selected.size,
                fn: () => batchAct((id) => api.reprintJob(id)),
              })
            }
          >
            <RotateCcw className="h-3.5 w-3.5" />
            {t('print.queue.action.reprint')}
          </Button>
          <Button
            variant="destructive"
            size="sm"
            className="h-7 gap-1 text-xs"
            onClick={() =>
              setBatchConfirm({
                type: 'cancel',
                count: selected.size,
                fn: () => batchAct((id) => api.cancelJob(id)),
              })
            }
          >
            <Ban className="h-3.5 w-3.5" />
            {t('print.queue.action.cancel')}
          </Button>
          {printerNames.length > 0 && (
            <Button
              variant="outline"
              size="sm"
              className="h-7 gap-1 text-xs"
              onClick={() =>
                setBatchConfirm({
                  type: 'pin',
                  count: selected.size,
                  fn: (printer) =>
                    batchAct((id) => api.pinJob(id, printer ?? null)),
                })
              }
            >
              <Crosshair className="h-3.5 w-3.5" />
              {t('print.queue.action.pin')}
            </Button>
          )}
        </>
      )}
    </div>
  );

  if (!user?.permissions.includes('contest:manage')) {
    return (
      <div className="flex h-[60vh] items-center justify-center p-6">
        <p className="text-sm text-muted-foreground">403 — staff access only</p>
      </div>
    );
  }

  return (
    <div className="mx-auto w-full max-w-7xl p-4 sm:p-6 space-y-4">
      <header className="flex items-center gap-3">
        <Printer className="h-6 w-6 text-primary shrink-0" />
        <div>
          <h1 className="text-2xl font-bold tracking-tight text-foreground">
            {t('print.queue.title')}
          </h1>
          <p className="text-sm text-muted-foreground mt-1">
            {t('print.queue.description')}
          </p>
        </div>
      </header>

      <Tabs defaultValue="jobs" className="space-y-3">
        <TabsList>
          <TabsTrigger value="jobs">{t('print.queue.tab.jobs')}</TabsTrigger>
          <TabsTrigger value="stations">
            {t('print.queue.tab.stations')}
          </TabsTrigger>
        </TabsList>

        <TabsContent value="jobs">
          <DataTable<PrintJob>
            columns={columns}
            queryKey={[
              'print',
              'jobs',
              statusFilter || 'all',
              stationFilter,
              printerFilter,
              String(nonce),
            ]}
            fetchFn={fetchJobs}
            searchable
            searchPlaceholder={t('print.queue.search')}
            defaultPerPage={PER_PAGE}
            defaultSortBy="created_at"
            defaultSortOrder="desc"
            emptyMessage={t('print.queue.empty')}
            toolbar={toolbar}
          />
        </TabsContent>

        <TabsContent value="stations">
          <StationsPanel />
        </TabsContent>
      </Tabs>

      {/* Batch reprint, cancel, and pin share this dialog */}
      <Dialog
        open={!!batchConfirm}
        onOpenChange={(o) => {
          if (!o) {
            setBatchConfirm(null);
            setPinPrinter('');
          }
        }}
      >
        <DialogContent className="sm:max-w-sm">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              {batchConfirm?.type === 'cancel' ? (
                <AlertTriangle className="h-5 w-5 text-destructive" />
              ) : batchConfirm?.type === 'pin' ? (
                <Crosshair className="h-5 w-5" />
              ) : (
                <RotateCcw className="h-5 w-5" />
              )}
              {batchConfirm?.type === 'cancel'
                ? t('print.queue.action.cancel')
                : batchConfirm?.type === 'pin'
                  ? t('print.queue.action.pin')
                  : t('print.queue.action.reprint')}
            </DialogTitle>
            <DialogDescription>
              {batchConfirm?.type === 'cancel'
                ? t('print.queue.action.confirmCancelBatch', {
                    count: batchConfirm?.count ?? 0,
                  })
                : batchConfirm?.type === 'pin'
                  ? t('print.queue.action.confirmPinBatch', {
                      count: batchConfirm?.count ?? 0,
                    })
                  : t('print.queue.action.confirmReprintBatch', {
                      count: batchConfirm?.count ?? 0,
                    })}
            </DialogDescription>
          </DialogHeader>
          {batchConfirm?.type === 'pin' && printerNames.length > 0 && (
            <Select value={pinPrinter} onValueChange={setPinPrinter}>
              <SelectTrigger className="h-8 text-sm">
                <SelectValue
                  placeholder={t('print.queue.action.pinPlaceholder')}
                />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="__clear__">
                  {t('print.queue.action.clearPin')}
                </SelectItem>
                {printerNames.map((p) => (
                  <SelectItem key={p} value={p}>
                    {p}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          )}
          <DialogFooter>
            <Button
              variant="ghost"
              onClick={() => {
                setBatchConfirm(null);
                setPinPrinter('');
              }}
            >
              {t('print.dialog.cancel')}
            </Button>
            <Button
              variant={
                batchConfirm?.type === 'cancel' ? 'destructive' : 'default'
              }
              disabled={batchConfirm?.type === 'pin' && !pinPrinter}
              onClick={() => {
                batchConfirm?.fn(
                  batchConfirm?.type === 'pin'
                    ? pinPrinter === '__clear__'
                      ? null
                      : pinPrinter
                    : undefined,
                );
                setBatchConfirm(null);
                setPinPrinter('');
              }}
            >
              {batchConfirm?.type === 'cancel'
                ? t('print.queue.action.cancel')
                : batchConfirm?.type === 'pin'
                  ? t('print.queue.action.pin')
                  : t('print.queue.action.reprint')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Cancel confirmation */}
      <Dialog
        open={!!cancelTarget}
        onOpenChange={(o) => {
          if (!o) setCancelTarget(null);
        }}
      >
        <DialogContent className="sm:max-w-sm">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <AlertTriangle className="h-5 w-5 text-destructive" />
              {t('print.queue.action.cancel')}
            </DialogTitle>
            <DialogDescription>
              {cancelTarget
                ? t('print.queue.action.confirmCancel', {
                    id: String(cancelTarget.id),
                    who: cancelTarget.display_name || cancelTarget.username,
                  })
                : ''}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="ghost" onClick={() => setCancelTarget(null)}>
              {t('print.dialog.cancel')}
            </Button>
            <Button
              variant="destructive"
              onClick={() => {
                act(async () => {
                  if (cancelTarget) await api.cancelJob(cancelTarget.id);
                });
                setCancelTarget(null);
              }}
            >
              {t('print.queue.action.cancel')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Reprint confirmation */}
      <Dialog
        open={!!reprintTarget}
        onOpenChange={(o) => {
          if (!o) setReprintTarget(null);
        }}
      >
        <DialogContent className="sm:max-w-sm">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <RotateCcw className="h-5 w-5" />
              {t('print.queue.action.reprint')}
            </DialogTitle>
            <DialogDescription>
              {reprintTarget
                ? t('print.queue.action.confirmReprint', {
                    id: String(reprintTarget.id),
                    who: reprintTarget.display_name || reprintTarget.username,
                  })
                : ''}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="ghost" onClick={() => setReprintTarget(null)}>
              {t('print.dialog.cancel')}
            </Button>
            <Button
              onClick={() => {
                act(async () => {
                  if (reprintTarget) await api.reprintJob(reprintTarget.id);
                });
                setReprintTarget(null);
              }}
            >
              {t('print.queue.action.reprint')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Code viewer */}
      <Dialog
        open={!!codeViewJob}
        onOpenChange={(o) => {
          if (!o) {
            setCodeViewJob(null);
            setCodeSource(null);
          }
        }}
      >
        <DialogContent className="sm:max-w-2xl">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2 font-mono text-sm">
              <Code2 className="h-4 w-4" />
              {codeViewJob
                ? t('print.queue.codeTitle', {
                    filename: codeViewJob.filename,
                    who: codeViewJob.display_name || codeViewJob.username,
                  })
                : ''}
            </DialogTitle>
            <DialogDescription>
              {codeViewJob?.language && (
                <span className="text-xs">{codeViewJob.language}</span>
              )}
            </DialogDescription>
          </DialogHeader>
          <Textarea
            className="h-96 resize-none font-mono text-xs leading-relaxed"
            value={codeSource ?? 'Loading…'}
            readOnly
            spellCheck={false}
          />
          <DialogFooter>
            <Button
              variant="ghost"
              onClick={() => {
                setCodeViewJob(null);
                setCodeSource(null);
              }}
            >
              {t('print.dialog.cancel')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
