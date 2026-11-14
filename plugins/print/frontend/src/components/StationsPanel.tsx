import { useFormat, useTranslation } from '@broccoli/web-sdk/i18n';
import { QueryView } from '@broccoli/web-sdk/ui';
import { cn } from '@broccoli/web-sdk/utils';
import { useQuery } from '@tanstack/react-query';
import { MonitorSmartphone, Printer } from 'lucide-react';

import { usePrintApi } from '../hooks/usePrintApi';
import { formatRelative } from '../lib/format';

export function StationsPanel() {
  const { t } = useTranslation();
  const format = useFormat();
  const api = usePrintApi();

  const query = useQuery({
    queryKey: ['print', 'stations'],
    queryFn: () => api.listStations(),
    refetchInterval: 10_000,
  });

  return (
    <QueryView
      query={query}
      failure={t('print.stations.loadFailed')}
      loading={
        <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
          {[0, 1, 2].map((i) => (
            <div
              key={i}
              className="h-28 animate-pulse rounded-xl border border-border bg-muted/40"
            />
          ))}
        </div>
      }
      isEmpty={(resp) => resp.data.length === 0}
      empty={
        <div className="flex flex-col items-center justify-center gap-3 rounded-xl border border-dashed border-border py-16 text-center">
          <MonitorSmartphone className="h-8 w-8 text-muted-foreground" />
          <p className="text-sm text-muted-foreground">
            {t('print.stations.empty')}
          </p>
        </div>
      }
    >
      {(resp) => (
        <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
          {resp.data.map((s) => {
            const printers = Array.isArray(s.printers) ? s.printers : [];
            return (
              <div
                key={s.name}
                className="rounded-xl border border-border bg-card p-4 shadow-sm transition-colors hover:border-foreground/20"
              >
                <div className="flex items-start justify-between gap-2">
                  <div className="min-w-0">
                    <p className="truncate font-medium text-foreground">
                      {s.name}
                    </p>
                    {s.location && (
                      <p className="truncate text-xs text-muted-foreground">
                        {s.location}
                      </p>
                    )}
                  </div>
                  <span
                    className={cn(
                      'inline-flex items-center gap-1.5 rounded-full border px-2 py-0.5 text-xs font-medium',
                      s.online
                        ? 'border-emerald-500/30 bg-emerald-500/12 text-emerald-600 dark:text-emerald-400'
                        : 'border-border bg-muted text-muted-foreground',
                    )}
                  >
                    <span className="relative flex h-1.5 w-1.5">
                      {s.online && (
                        <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-emerald-500 opacity-60" />
                      )}
                      <span
                        className={cn(
                          'relative inline-flex h-1.5 w-1.5 rounded-full',
                          s.online
                            ? 'bg-emerald-500'
                            : 'bg-muted-foreground/50',
                        )}
                      />
                    </span>
                    {s.online
                      ? t('print.stations.online')
                      : t('print.stations.offline')}
                  </span>
                </div>

                <div className="mt-3 flex flex-wrap gap-1.5">
                  {printers.length === 0 ? (
                    <span className="text-xs text-muted-foreground">—</span>
                  ) : (
                    printers.map((p) => (
                      <span
                        key={p}
                        className="inline-flex items-center gap-1 rounded-md border border-border bg-muted/50 px-1.5 py-0.5 font-mono text-[11px] text-foreground"
                      >
                        <Printer className="h-3 w-3 text-muted-foreground" />
                        {p}
                      </span>
                    ))
                  )}
                </div>

                <div className="mt-3 flex items-center justify-between text-xs text-muted-foreground tabular-nums">
                  <span>{formatRelative(format, s.last_seen)}</span>
                  {s.version && (
                    <span className="font-mono">
                      {t('print.stations.version', { version: s.version })}
                    </span>
                  )}
                </div>
              </div>
            );
          })}
        </div>
      )}
    </QueryView>
  );
}
