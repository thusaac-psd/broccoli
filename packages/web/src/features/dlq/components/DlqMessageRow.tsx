import { useFormat, useTranslation } from '@broccoli/web-sdk/i18n';
import { Badge } from '@broccoli/web-sdk/ui';

import type { DlqMessage } from '@/features/dlq/types';
import { messageTypeMeta } from '@/features/dlq/utils/messageType';

interface Props {
  message: DlqMessage;
  onClick: (id: number) => void;
}

export function DlqMessageRow({ message, onClick }: Props) {
  const { t } = useTranslation();
  const format = useFormat();
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
        {format.relative(message.last_failed_at)}
      </td>
      <td className="p-3 align-top">
        {message.resolved ? (
          <Badge variant="secondary">
            {message.resolved_by == null
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
