import { useTranslation } from '@broccoli/web-sdk/i18n';
import { cn } from '@broccoli/web-sdk/utils';

import { STATUS_META } from '../lib/format';
import type { PrintStatus } from '../types';

export function StatusPill({ status }: { status: PrintStatus }) {
  const { t } = useTranslation();
  const meta = STATUS_META[status] ?? STATUS_META.pending;

  return (
    <span
      className={cn(
        'inline-flex items-center gap-1.5 rounded-full border px-2.5 py-0.5 text-xs font-medium whitespace-nowrap',
        meta.pill,
      )}
    >
      <span className="relative flex h-1.5 w-1.5">
        {meta.pulse && (
          <span
            className={cn(
              'absolute inline-flex h-full w-full animate-ping rounded-full opacity-60',
              meta.dot,
            )}
          />
        )}
        <span
          className={cn(
            'relative inline-flex h-1.5 w-1.5 rounded-full',
            meta.dot,
          )}
        />
      </span>
      {t(meta.labelKey)}
    </span>
  );
}
