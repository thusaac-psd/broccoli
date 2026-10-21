import { getErrorMessage, useApiClient } from '@broccoli/web-sdk/api';
import type { CreatedUser } from '@broccoli/web-sdk/auth';
import { useIdempotencyKey } from '@broccoli/web-sdk/hooks';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Input,
  Label,
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
  Textarea,
} from '@broccoli/web-sdk/ui';
import { useQueryClient } from '@tanstack/react-query';
import { Download, Loader2 } from 'lucide-react';
import { useId, useState } from 'react';
import { toast } from 'sonner';

import { DisplayNameField } from '@/components/DisplayNameField';
import {
  normalizeBulkUsers,
  type ParsedBulkUser,
} from '@/features/user-management/lib/bulk-users';
import { downloadUserCredentials } from '@/features/user-management/lib/user-credentials';

export function CreateUsersDialog({
  onOpenChange,
}: {
  onOpenChange: (open: boolean) => void;
}) {
  const { t } = useTranslation();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();
  const { getKey, resetKey } = useIdempotencyKey();
  const formId = useId();
  const [tab, setTab] = useState('single');
  const [username, setUsername] = useState('');
  const [displayName, setDisplayName] = useState('');
  const [password, setPassword] = useState('');
  const [jsonText, setJsonText] = useState('');
  const [preview, setPreview] = useState<ParsedBulkUser[] | null>(null);
  const [created, setCreated] = useState<CreatedUser[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [errorMsg, setErrorMsg] = useState('');

  function clearRequest() {
    resetKey();
    setErrorMsg('');
    setPreview(null);
  }

  function parseUsers(input: unknown) {
    if (Array.isArray(input) && input.length > 100)
      throw new Error('users.create.tooMany');
    return normalizeBulkUsers(input);
  }

  function showValidation(error: unknown) {
    const message = error instanceof Error ? error.message : '';
    const key =
      message.startsWith('admin.bulkParticipants') ||
      message === 'users.create.tooMany'
        ? message
        : 'admin.bulkParticipantsInvalidJson';
    setErrorMsg(t(key));
  }

  async function submit(users: ParsedBulkUser[]) {
    if (busy || created) return;
    setBusy(true);
    setErrorMsg('');
    try {
      const options = { headers: { 'Idempotency-Key': getKey() } };
      let result: CreatedUser[];
      if (tab === 'single') {
        const { data, error } = await apiClient.POST('/users', {
          ...options,
          body: users[0],
        });
        if (error) throw error;
        result = [data];
      } else {
        const { data, error } = await apiClient.POST('/users/bulk', {
          ...options,
          body: { users },
        });
        if (error) throw error;
        result = data;
      }
      setCreated(result);
      resetKey();
      toast.success(
        t('users.create.success', { count: String(result.length) }),
      );
      queryClient.invalidateQueries({ queryKey: ['admin-users'] });
      queryClient.invalidateQueries({
        queryKey: ['all-users-for-participants'],
      });
    } catch (error) {
      setErrorMsg(getErrorMessage(error, t('users.create.error')));
    } finally {
      setBusy(false);
    }
  }

  async function readFile(event: React.ChangeEvent<HTMLInputElement>) {
    const file = event.target.files?.[0];
    event.target.value = '';
    if (!file) return;
    clearRequest();
    if (file.size > 100_000) {
      setErrorMsg(t('users.create.fileTooLarge'));
      return;
    }
    try {
      setJsonText(await file.text());
    } catch {
      setErrorMsg(t('users.create.error'));
    }
  }

  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!busy) onOpenChange(open);
      }}
    >
      <DialogContent className="max-w-xl max-h-[90vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>{t('users.create.title')}</DialogTitle>
          <DialogDescription>
            {t(
              created
                ? 'users.create.credentialsHint'
                : 'users.create.description',
            )}
          </DialogDescription>
        </DialogHeader>
        {created ? (
          <div className="space-y-4">
            <p role="status">
              {t('users.create.success', { count: String(created.length) })}
            </p>
            <div className="max-h-64 overflow-auto rounded-md border">
              <table className="w-full text-sm">
                <thead>
                  <tr className="border-b bg-muted/40">
                    <th className="p-2 text-start">{t('auth.username')}</th>
                    <th className="p-2 text-start">{t('auth.displayName')}</th>
                    <th className="p-2 text-start">
                      {t('admin.field.password')}
                    </th>
                  </tr>
                </thead>
                <tbody>
                  {created.map((user) => (
                    <tr key={user.id} className="border-b last:border-0">
                      <td className="p-2">{user.username}</td>
                      <td className="p-2">{user.display_name}</td>
                      <td className="p-2 font-mono select-all">
                        {user.password}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            <DialogFooter>
              <Button
                variant="outline"
                onClick={() => downloadUserCredentials(created)}
              >
                <Download className="h-4 w-4 mr-2" />
                {t('users.create.download')}
              </Button>
              <Button onClick={() => onOpenChange(false)}>
                {t('users.create.done')}
              </Button>
            </DialogFooter>
          </div>
        ) : (
          <Tabs
            value={tab}
            onValueChange={(value) => {
              if (!busy) {
                setTab(value);
                clearRequest();
              }
            }}
          >
            <TabsList className="grid w-full grid-cols-2">
              <TabsTrigger value="single" disabled={busy}>
                {t('users.create.single')}
              </TabsTrigger>
              <TabsTrigger value="bulk" disabled={busy}>
                {t('users.create.bulk')}
              </TabsTrigger>
            </TabsList>
            <TabsContent value="single">
              <form
                className="space-y-4"
                onSubmit={(event) => {
                  event.preventDefault();
                  try {
                    void submit(
                      parseUsers([
                        {
                          username,
                          display_name: displayName,
                          password: password || undefined,
                        },
                      ]),
                    );
                  } catch (error) {
                    showValidation(error);
                  }
                }}
              >
                <div className="space-y-2">
                  <Label htmlFor={`${formId}-username`}>
                    {t('auth.username')}
                  </Label>
                  <Input
                    id={`${formId}-username`}
                    required
                    maxLength={32}
                    pattern="[A-Za-z0-9_]+"
                    autoComplete="off"
                    value={username}
                    disabled={busy}
                    onChange={(e) => {
                      setUsername(e.target.value);
                      clearRequest();
                    }}
                  />
                </div>
                <DisplayNameField
                  id={`${formId}-display-name`}
                  value={displayName}
                  disabled={busy}
                  onChange={(e) => {
                    setDisplayName(e.target.value);
                    clearRequest();
                  }}
                />
                <div className="space-y-2">
                  <Label htmlFor={`${formId}-password`}>
                    {t('users.users.passwordOptional')}
                  </Label>
                  <Input
                    id={`${formId}-password`}
                    type="password"
                    autoComplete="new-password"
                    maxLength={128}
                    value={password}
                    disabled={busy}
                    onChange={(e) => {
                      setPassword(e.target.value);
                      clearRequest();
                    }}
                  />
                  <p className="text-xs text-muted-foreground">
                    {t('users.create.passwordHint')}
                  </p>
                </div>
                <DialogFooter>
                  <Button
                    variant="outline"
                    type="button"
                    disabled={busy}
                    onClick={() => onOpenChange(false)}
                  >
                    {t('users.common.cancel')}
                  </Button>
                  <Button type="submit" disabled={busy}>
                    {busy && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
                    {t('users.create.single')}
                  </Button>
                </DialogFooter>
              </form>
            </TabsContent>
            <TabsContent value="bulk" className="space-y-3">
              <Label htmlFor={`${formId}-file`}>
                {t('admin.bulkParticipantsJsonLabel')}
              </Label>
              <Input
                id={`${formId}-file`}
                type="file"
                accept=".json,application/json"
                disabled={busy}
                onChange={readFile}
              />
              <Label htmlFor={`${formId}-json`}>
                {t('users.create.bulkHint')}
              </Label>
              <Textarea
                id={`${formId}-json`}
                value={jsonText}
                rows={6}
                maxLength={100_000}
                disabled={busy}
                placeholder={t('admin.bulkParticipantsJsonPlaceholder')}
                onChange={(e) => {
                  setJsonText(e.target.value);
                  clearRequest();
                }}
              />
              <Button
                variant="outline"
                disabled={busy || !jsonText.trim()}
                onClick={() => {
                  clearRequest();
                  try {
                    setPreview(parseUsers(JSON.parse(jsonText)));
                  } catch (error) {
                    showValidation(error);
                  }
                }}
              >
                {t('admin.bulkParticipantsPreview')}
              </Button>
              {preview && (
                <div className="space-y-3 rounded-md border p-3">
                  <p>
                    {t('users.create.preview', {
                      count: String(preview.length),
                    })}
                  </p>
                  <div className="max-h-48 overflow-auto text-sm">
                    {preview.map((entry) => (
                      <div
                        key={entry.username}
                        className="flex justify-between gap-4 py-1"
                      >
                        <span>
                          {entry.display_name}{' '}
                          <span className="text-muted-foreground">
                            ({entry.username})
                          </span>
                        </span>
                        <span className="text-muted-foreground">
                          {t(
                            entry.password
                              ? 'users.create.suppliedPassword'
                              : 'admin.bulkParticipantsAutoPassword',
                          )}
                        </span>
                      </div>
                    ))}
                  </div>
                  <Button disabled={busy} onClick={() => void submit(preview)}>
                    {busy && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
                    {t('users.create.confirm')}
                  </Button>
                </div>
              )}
            </TabsContent>
          </Tabs>
        )}
        {errorMsg && (
          <p role="alert" className="text-sm text-destructive">
            {errorMsg}
          </p>
        )}
      </DialogContent>
    </Dialog>
  );
}
