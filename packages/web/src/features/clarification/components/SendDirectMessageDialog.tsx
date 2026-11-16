import { useApiClient } from '@broccoli/web-sdk/api';
import { queryState } from '@broccoli/web-sdk/hooks';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Button, Input, QueryErrorState } from '@broccoli/web-sdk/ui';
import { useQuery } from '@tanstack/react-query';
import { Check, Mail, Search } from 'lucide-react';
import { useMemo, useState } from 'react';

import { ComposeMessageDialog } from '@/features/clarification/components/ComposeMessageDialog';

interface Recipient {
  user_id: number;
  username: string;
}

export function SendDirectMessageDialog({
  contestId,
  onSubmit,
}: {
  contestId: number;
  onSubmit: (content: string, recipientId: number) => Promise<unknown>;
}) {
  const { t } = useTranslation();
  const [recipient, setRecipient] = useState<Recipient | null>(null);

  return (
    <ComposeMessageDialog
      trigger={
        <Button variant="outline">
          <Mail className="h-4 w-4 mr-2" />
          {t('clarification.dm.trigger')}
        </Button>
      }
      title={t('clarification.dm.title')}
      placeholder={t('clarification.dm.placeholder')}
      hint={t('clarification.dm.hint')}
      submitLabel={t('clarification.send')}
      ready={recipient !== null}
      onSubmit={(content) => onSubmit(content, recipient!.user_id)}
      onSent={() => setRecipient(null)}
    >
      <RecipientPicker
        contestId={contestId}
        recipient={recipient}
        onChange={setRecipient}
      />
    </ComposeMessageDialog>
  );
}

function RecipientPicker({
  contestId,
  recipient,
  onChange,
}: {
  contestId: number;
  recipient: Recipient | null;
  onChange: (recipient: Recipient | null) => void;
}) {
  const { t } = useTranslation();
  const apiClient = useApiClient();
  const [search, setSearch] = useState('');

  const participantsQuery = useQuery({
    queryKey: ['contest-participants', contestId],
    queryFn: async () => {
      const { data, error } = await apiClient.GET(
        '/contests/{id}/participants',
        { params: { path: { id: contestId } } },
      );
      if (error) throw error;
      return data;
    },
  });
  const participantsState = queryState(participantsQuery);

  const filtered = useMemo(() => {
    const term = search.trim().toLowerCase();
    return (participantsQuery.data ?? []).filter(
      (p) =>
        !p.is_deleted &&
        (!term ||
          p.username.toLowerCase().includes(term) ||
          String(p.user_id).includes(term)),
    );
  }, [participantsQuery.data, search]);

  return (
    <div className="space-y-2">
      <span className="text-sm font-medium">
        {t('clarification.dm.recipient')}
      </span>
      {recipient ? (
        <div className="flex items-center gap-2 p-2 border rounded-md bg-muted/50">
          <Check className="h-4 w-4 text-green-600" />
          <span className="font-medium">{recipient.username}</span>
          <span className="text-xs text-muted-foreground">
            {t('clarification.dm.userId', { id: recipient.user_id })}
          </span>
          <Button
            variant="ghost"
            size="sm"
            className="ml-auto h-6 text-xs"
            onClick={() => onChange(null)}
          >
            {t('clarification.dm.change')}
          </Button>
        </div>
      ) : (
        <>
          <div className="relative">
            <Search
              className="pointer-events-none absolute top-2.5 h-4 w-4 text-muted-foreground"
              style={{ insetInlineStart: '0.625rem' }}
            />
            <Input
              placeholder={t('clarification.dm.searchPlaceholder')}
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              style={{ paddingInlineStart: '2.25rem' }}
            />
          </div>
          <div className="max-h-36 overflow-y-auto border rounded-md">
            {participantsState.status === 'error' ? (
              <QueryErrorState
                className="py-4"
                error={participantsState.error}
                retry={participantsState.retry}
                retrying={participantsState.retrying}
                failure={t('admin.participants.loadFailed')}
              />
            ) : participantsState.status === 'loading' ? (
              <div className="p-3 text-sm text-muted-foreground text-center">
                {t('common.loading')}
              </div>
            ) : filtered.length === 0 ? (
              <div className="p-3 text-sm text-muted-foreground text-center">
                {t('clarification.dm.noParticipants')}
              </div>
            ) : (
              filtered.map((p) => (
                <button
                  key={p.user_id}
                  type="button"
                  className="w-full px-3 py-2 text-left text-sm hover:bg-muted flex items-center justify-between"
                  onClick={() => onChange(p)}
                >
                  <span>{p.username}</span>
                  <span className="text-xs text-muted-foreground">
                    {t('clarification.dm.userId', { id: p.user_id })}
                  </span>
                </button>
              ))
            )}
          </div>
        </>
      )}
    </div>
  );
}
