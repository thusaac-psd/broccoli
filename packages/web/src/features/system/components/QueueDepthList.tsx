import { useTranslation } from '@broccoli/web-sdk/i18n';

import type { QueueInfo } from '@/features/system/types';

interface Props {
  queues: QueueInfo[];
}

const RESULT_QUEUE_HINT = 'result';

const TONE = {
  idle: 'text-muted-foreground',
  backlog: 'text-destructive',
  busy: 'text-foreground',
};

export function QueueDepthList({ queues }: Props) {
  const { t } = useTranslation();

  if (queues.length === 0) {
    return (
      <p className="text-sm text-muted-foreground">
        {t('system.queues.empty')}
      </p>
    );
  }

  return (
    <ul className="divide-y rounded-lg border bg-card">
      {queues.map((q) => {
        const isResultQueue = q.name.toLowerCase().includes(RESULT_QUEUE_HINT);
        const tone =
          q.depth === 0 ? TONE.idle : isResultQueue ? TONE.backlog : TONE.busy;

        return (
          <li key={q.name} className="flex items-center justify-between p-3">
            <div className="min-w-0">
              <div className="font-mono text-xs truncate">{q.name}</div>
              {Object.keys(q.breakdown).length > 0 && (
                <div className="mt-0.5 flex flex-wrap gap-x-2 gap-y-0.5 text-[10px] text-muted-foreground">
                  {Object.entries(q.breakdown).map(([state, count]) => (
                    <span key={state} className="tabular-nums">
                      {state}: {count}
                    </span>
                  ))}
                </div>
              )}
            </div>
            <span className={`text-lg font-semibold tabular-nums ${tone}`}>
              {q.depth}
            </span>
          </li>
        );
      })}
    </ul>
  );
}
