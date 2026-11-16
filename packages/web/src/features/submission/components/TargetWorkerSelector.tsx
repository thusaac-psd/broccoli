import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Badge, Button } from '@broccoli/web-sdk/ui';
import { Pin, X } from 'lucide-react';
import { useMemo, useState } from 'react';

import { useSystemOverview } from '@/features/system/hooks/useSystemOverview';

interface Props {
  selected: string[];
  onChange: (next: string[]) => void;
  disabled?: boolean;
}

/**
 * Admin-only chip multi-select for pinning a submission to specific workers.
 * Renders nothing if no live workers are visible. The parent component is
 * responsible for permission-gating; this component assumes it should render
 * when mounted.
 */
export function TargetWorkerSelector({ selected, onChange, disabled }: Props) {
  const { t } = useTranslation();
  const { data, isLoading } = useSystemOverview();
  const [open, setOpen] = useState(false);

  const liveWorkers = useMemo(() => data?.workers ?? [], [data]);

  const available = useMemo(
    () => liveWorkers.filter((w) => !selected.includes(w.id)),
    [liveWorkers, selected],
  );

  function add(id: string) {
    onChange([...selected, id]);
    setOpen(false);
  }

  function remove(id: string) {
    onChange(selected.filter((x) => x !== id));
  }

  if (!isLoading && liveWorkers.length === 0) {
    // No reason to show the picker when there are no live workers to choose.
    return null;
  }

  return (
    <div className="flex flex-wrap items-center gap-2 rounded-md border border-dashed bg-muted/20 px-3 py-2 text-xs">
      <span className="inline-flex items-center gap-1 font-medium text-muted-foreground">
        <Pin className="h-3 w-3" />
        {t('submit.pinTo')}
      </span>

      {selected.length === 0 && (
        <span className="text-muted-foreground">{t('submit.pinToHint')}</span>
      )}

      {selected.map((id) => (
        <Badge
          key={id}
          variant="outline"
          className="gap-1 font-mono text-[11px] py-0.5"
        >
          {id}
          <button
            type="button"
            onClick={() => remove(id)}
            disabled={disabled}
            className="ml-0.5 rounded-sm hover:bg-muted/60"
            aria-label={t('submit.removeWorker', { id })}
          >
            <X className="h-3 w-3" />
          </button>
        </Badge>
      ))}

      {available.length > 0 && (
        <div className="relative">
          <Button
            type="button"
            variant="ghost"
            size="sm"
            disabled={disabled}
            onClick={() => setOpen((v) => !v)}
            className="h-6 px-2 text-[11px]"
          >
            {selected.length === 0
              ? t('submit.addWorker')
              : t('submit.addAnother')}
          </Button>
          {open && (
            <div className="absolute z-10 mt-1 w-56 rounded-md border bg-popover p-1 shadow-md">
              {available.map((w) => (
                <button
                  key={w.id}
                  type="button"
                  onClick={() => add(w.id)}
                  className="block w-full rounded-sm px-2 py-1 text-left hover:bg-muted"
                >
                  <div className="font-mono text-xs font-medium">{w.id}</div>
                  <div className="text-[10px] text-muted-foreground">
                    {w.hostname ?? '—'} · {w.os ?? '?'}/{w.arch ?? '?'}
                  </div>
                </button>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
