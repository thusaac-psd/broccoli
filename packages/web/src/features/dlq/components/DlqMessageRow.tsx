import { serverNow } from '@broccoli/web-sdk/api';
import { type I18nContextValue, useTranslation } from '@broccoli/web-sdk/i18n';
import { Badge } from '@broccoli/web-sdk/ui';

import type { DlqMessage } from '@/features/dlq/types';
import { messageTypeMeta } from '@/features/dlq/utils/messageType';

interface Props {
  message: DlqMessage;
  onClick: (id: number) => void;
}

function timeAgo(iso: string, t: I18nContextValue['t']) {
  const seconds = Math.max(
    0,
    Math.floor((serverNow() - Date.parse(iso)) / 1000),
  );
  if (seconds < 60) return t('dlq.row.secondsAgo', { count: seconds });
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return t('dlq.row.minutesAgo', { count: minutes });
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return t('dlq.row.hoursAgo', { count: hours });
  const days = Math.floor(hours / 24);
  return t('dlq.row.daysAgo', { count: days });
}

export function DlqMessageRow({ message, onClick }: Props) {
  const { t } = useTranslation();
  const meta = messageTypeMeta(message.message_type);
  const Icon = meta.icon;

  return (
    <tr
      onClick={() => onClick(message.id)}
      className="cursor-pointer border-b transition-colors hover:bg-muted/40"
    >
      <td className="p-3 align-top">
        <div className="flex flex-col gap-1">
          <span className="font-mono text-xs truncate max-w-[16ch]">
            {message.message_id}
          </span>
          {message.submission_id != null && (
            <span className="text-[10px] text-muted-foreground">
              {t('dlq.row.submission', { id: message.submission_id })}
            </span>
          )}
        </div>
      </td>
      <td className="p-3 align-top">
        <span className="inline-flex items-center gap-1 rounded-md border bg-muted/30 px-1.5 py-0.5 text-[10px]">
          <Icon className="h-3 w-3 text-muted-foreground" />
          {t(meta.labelKey)}
        </span>
      </td>
      <td className="p-3 align-top">
        <div className="flex flex-col gap-0.5">
          <span className="font-medium text-xs">{message.error_code}</span>
          <span className="text-xs text-muted-foreground line-clamp-2">
            {message.error_message}
          </span>
        </div>
      </td>
      <td className="p-3 align-top text-center text-sm tabular-nums">
        {message.retry_count}
      </td>
      <td className="p-3 align-top text-xs text-muted-foreground tabular-nums whitespace-nowrap">
        {timeAgo(message.last_failed_at, t)}
      </td>
      <td className="p-3 align-top">
        {message.resolved ? (
          <Badge variant="secondary">
            {message.resolved_by === null
              ? t('dlq.row.recovered')
              : t('dlq.row.resolved')}
          </Badge>
        ) : (
          <Badge
            variant="outline"
            className="border-destructive/50 text-destructive"
          >
            {t('dlq.row.unresolved')}
          </Badge>
        )}
      </td>
    </tr>
  );
}
