import {
  type ApiClient,
  getErrorMessage,
  useApiClient,
} from '@broccoli/web-sdk/api';
import type { ContestSummary } from '@broccoli/web-sdk/contest';
import { useIdempotencyKey } from '@broccoli/web-sdk/hooks';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import {
  Badge,
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
import { formatDateTime } from '@broccoli/web-sdk/utils';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import {
  Download,
  Search,
  Upload,
  UserMinus,
  UserPlus,
  Users,
} from 'lucide-react';
import { useEffect, useMemo, useState } from 'react';
import { toast } from 'sonner';

import {
  normalizeBulkUsers,
  type ParsedBulkUser,
} from '@/features/user-management/lib/bulk-users';
import { downloadUserCredentials } from '@/features/user-management/lib/user-credentials';
import type { ManagedUserRow } from '@/features/user-management/types';

// -- Types --

type UserPreviewItem = ManagedUserRow;

type ParticipantItem = {
  user_id: number;
  username: string;
  display_name: string;
  is_deleted: boolean;
  registered_at: string;
};

type ParticipantIdentity = Pick<
  ParticipantItem,
  'user_id' | 'username' | 'display_name'
>;

// -- Helpers --

async function fetchParticipants(apiClient: ApiClient, contestId: number) {
  const { data, error } = await apiClient.GET('/contests/{id}/participants', {
    params: { path: { id: contestId } },
  });
  if (error) throw error;
  return data;
}

async function fetchAllUsers(apiClient: ApiClient) {
  const { data, error } = await apiClient.GET('/users');
  if (error) throw error;
  return data;
}

// -- Enrolled Participants Tab --

function EnrolledTab({
  contest,
  participants,
  isLoading,
}: {
  contest: ContestSummary;
  participants: ParticipantItem[];
  isLoading: boolean;
}) {
  const { t, locale } = useTranslation();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();
  const [search, setSearch] = useState('');
  const [removingId, setRemovingId] = useState<number | null>(null);

  const filtered = useMemo(() => {
    if (!search) return participants;
    const q = search.toLowerCase();
    return participants.filter(
      (p) =>
        p.username.toLowerCase().includes(q) ||
        p.display_name.toLowerCase().includes(q) ||
        String(p.user_id).includes(q),
    );
  }, [participants, search]);

  async function handleRemove(userId: number, username: string) {
    if (!window.confirm(t('admin.participants.removeConfirm', { username })))
      return;
    setRemovingId(userId);
    const { error } = await apiClient.DELETE(
      '/contests/{id}/participants/{user_id}',
      {
        params: { path: { id: contest.id, user_id: userId } },
      },
    );
    setRemovingId(null);
    if (error) {
      toast.error(getErrorMessage(error, t('toast.participant.removeError')));
    } else {
      toast.success(t('toast.participant.removed'));
      queryClient.invalidateQueries({
        queryKey: ['contest-participants', contest.id],
      });
    }
  }

  return (
    <div className="space-y-3">
      <div className="flex items-center gap-2">
        <div className="relative flex-1">
          <Search
            className="pointer-events-none absolute top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground"
            style={{ insetInlineStart: '0.625rem' }}
          />
          <Input
            placeholder={t('admin.participants.searchEnrolled')}
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            className="h-8 text-sm"
            style={{ paddingInlineStart: '2rem' }}
          />
        </div>
        <Badge variant="secondary" className="text-xs whitespace-nowrap">
          {t('admin.participants.totalCount', {
            count: String(participants.length),
          })}
        </Badge>
      </div>

      <div className="overflow-y-auto max-h-[420px] rounded-md border">
        {isLoading ? (
          <div className="py-8 text-center text-muted-foreground">
            {t('admin.loading')}
          </div>
        ) : filtered.length === 0 ? (
          <div className="py-8 text-center text-sm text-muted-foreground">
            {participants.length === 0
              ? t('admin.participants.empty')
              : t('admin.participants.noSearchResults')}
          </div>
        ) : (
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b bg-muted/40">
                <th className="px-3 py-2 text-left font-medium text-foreground/80 w-16">
                  #
                </th>
                <th className="px-3 py-2 text-left font-medium text-foreground/80">
                  {t('auth.username')}
                </th>
                <th className="px-3 py-2 text-left font-medium text-foreground/80">
                  {t('auth.displayName')}
                </th>
                <th className="px-3 py-2 text-left font-medium text-foreground/80">
                  {t('admin.participants.registeredAt')}
                </th>
                <th className="px-3 py-2 text-right font-medium text-foreground/80 w-20" />
              </tr>
            </thead>
            <tbody>
              {filtered.map((p) => (
                <tr
                  key={p.user_id}
                  className="border-b last:border-0 hover:bg-muted/30"
                >
                  <td className="px-3 py-2 text-muted-foreground">
                    {p.user_id}
                  </td>
                  <td className="px-3 py-2 font-medium">
                    {p.is_deleted ? (
                      <span className="text-muted-foreground italic">
                        [Deleted User]
                      </span>
                    ) : (
                      p.username
                    )}
                  </td>
                  <td className="px-3 py-2 text-muted-foreground">
                    {p.is_deleted ? '—' : p.display_name}
                  </td>
                  <td className="px-3 py-2 text-muted-foreground">
                    {formatDateTime(p.registered_at, locale)}
                  </td>
                  <td className="px-3 py-2 text-right">
                    <Button
                      variant="ghost"
                      size="icon"
                      className="h-7 w-7 text-destructive hover:text-destructive"
                      disabled={removingId === p.user_id}
                      onClick={() => handleRemove(p.user_id, p.username)}
                    >
                      <UserMinus className="h-3.5 w-3.5" />
                    </Button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </div>
  );
}

// -- Add Participants Tab --

function AddParticipantsTab({
  contest,
  participants,
}: {
  contest: ContestSummary;
  participants: ParticipantItem[];
}) {
  const { t } = useTranslation();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();
  const { getKey, resetKey } = useIdempotencyKey();
  const [search, setSearch] = useState('');
  const [addingId, setAddingId] = useState<number | null>(null);

  const { data: allUsers = [], isLoading: loadingUsers } = useQuery({
    queryKey: ['all-users-for-participants'],
    queryFn: () => fetchAllUsers(apiClient),
  });

  const enrolledIds = useMemo(
    () => new Set(participants.map((p) => p.user_id)),
    [participants],
  );

  const unenrolledUsers = useMemo(() => {
    return allUsers.filter((u) => !enrolledIds.has(u.id));
  }, [allUsers, enrolledIds]);

  const filteredUsers = useMemo(() => {
    if (!search) return unenrolledUsers;
    const q = search.toLowerCase();
    return unenrolledUsers.filter(
      (u) =>
        u.username.toLowerCase().includes(q) ||
        u.display_name.toLowerCase().includes(q) ||
        String(u.id).includes(q),
    );
  }, [unenrolledUsers, search]);

  async function handleAdd(userId: number) {
    setAddingId(userId);
    const { error } = await apiClient.POST('/contests/{id}/participants', {
      headers: { 'Idempotency-Key': getKey() },
      params: { path: { id: contest.id } },
      body: { user_id: userId },
    });
    setAddingId(null);
    if (error) {
      toast.error(getErrorMessage(error, t('toast.participant.addError')));
    } else {
      resetKey();
      toast.success(t('toast.participant.added'));
      queryClient.invalidateQueries({
        queryKey: ['contest-participants', contest.id],
      });
    }
  }

  return (
    <div className="space-y-3">
      <div className="flex items-center gap-2">
        <div className="relative flex-1">
          <Search
            className="pointer-events-none absolute top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground"
            style={{ insetInlineStart: '0.625rem' }}
          />
          <Input
            placeholder={t('admin.participants.searchUsers')}
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            className="h-8 text-sm"
            style={{ paddingInlineStart: '2rem' }}
          />
        </div>
        <Badge variant="secondary" className="text-xs whitespace-nowrap">
          {t('admin.participants.unenrolledCount', {
            count: String(unenrolledUsers.length),
          })}
        </Badge>
      </div>

      <div className="overflow-y-auto max-h-[420px] rounded-md border">
        {loadingUsers ? (
          <div className="py-8 text-center text-muted-foreground">
            {t('admin.loading')}
          </div>
        ) : filteredUsers.length === 0 ? (
          <div className="py-8 text-center text-sm text-muted-foreground">
            {unenrolledUsers.length === 0
              ? t('admin.participants.allEnrolled')
              : t('admin.participants.noSearchResults')}
          </div>
        ) : (
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b bg-muted/40">
                <th className="px-3 py-2 text-left font-medium text-foreground/80 w-16">
                  #
                </th>
                <th className="px-3 py-2 text-left font-medium text-foreground/80">
                  {t('auth.username')}
                </th>
                <th className="px-3 py-2 text-left font-medium text-foreground/80">
                  {t('auth.displayName')}
                </th>
                <th className="px-3 py-2 text-left font-medium text-foreground/80">
                  {t('admin.field.role')}
                </th>
                <th className="px-3 py-2 text-right font-medium text-foreground/80 w-20" />
              </tr>
            </thead>
            <tbody>
              {filteredUsers.map((u) => (
                <tr
                  key={u.id}
                  className="border-b last:border-0 hover:bg-muted/30"
                >
                  <td className="px-3 py-2 text-muted-foreground">{u.id}</td>
                  <td className="px-3 py-2 font-medium">{u.username}</td>
                  <td className="px-3 py-2">{u.display_name}</td>
                  <td className="px-3 py-2 text-muted-foreground">
                    {u.roles.join(', ')}
                  </td>
                  <td className="px-3 py-2 text-right">
                    <Button
                      variant="outline"
                      size="sm"
                      className="h-7 text-xs"
                      disabled={addingId === u.id}
                      onClick={() => handleAdd(u.id)}
                    >
                      <UserPlus className="h-3 w-3 mr-1" />
                      {addingId === u.id
                        ? t('admin.adding')
                        : t('admin.participants.add')}
                    </Button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </div>
  );
}

// -- Bulk Import Tab --

function PreviewUserTable({
  title,
  users,
}: {
  title: string;
  users: UserPreviewItem[];
}) {
  const { t } = useTranslation();
  if (users.length === 0) return null;
  return (
    <div className="rounded-md border overflow-hidden">
      <div className="px-3 py-2 border-b bg-muted/40 text-sm font-medium">
        {title}
      </div>
      <table className="w-full text-sm">
        <thead>
          <tr className="border-b bg-muted/20">
            <th className="px-3 py-2 text-left font-medium text-foreground/80">
              #
            </th>
            <th className="px-3 py-2 text-left font-medium text-foreground/80">
              {t('auth.username')}
            </th>
            <th className="px-3 py-2 text-left font-medium text-foreground/80">
              {t('auth.displayName')}
            </th>
            <th className="px-3 py-2 text-left font-medium text-foreground/80">
              {t('admin.field.role')}
            </th>
          </tr>
        </thead>
        <tbody>
          {users.map((user) => (
            <tr
              key={`${title}-${user.id}-${user.username}`}
              className="border-b last:border-0"
            >
              <td className="px-3 py-2">{user.id}</td>
              <td className="px-3 py-2">{user.username}</td>
              <td className="px-3 py-2">{user.display_name}</td>
              <td className="px-3 py-2">{user.roles.join(', ')}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function BulkImportTab({
  contest,
  participants,
}: {
  contest: ContestSummary;
  participants: ParticipantItem[];
}) {
  const { t } = useTranslation();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();
  const { getKey, resetKey } = useIdempotencyKey();
  const [jsonText, setJsonText] = useState('');
  const [loadingPreview, setLoadingPreview] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [errorMsg, setErrorMsg] = useState('');
  const [result, setResult] = useState<{
    added: ParticipantIdentity[];
    created: (ParticipantIdentity & { password: string })[];
    already_enrolled: ParticipantIdentity[];
    not_found: string[];
  } | null>(null);
  const [preview, setPreview] = useState<{
    willCreate: ParsedBulkUser[];
    willAdd: UserPreviewItem[];
    alreadyEnrolled: UserPreviewItem[];
    existingWithPassword: UserPreviewItem[];
  } | null>(null);

  async function handleReadJsonFile(e: React.ChangeEvent<HTMLInputElement>) {
    const file = e.target.files?.[0];
    if (!file) return;
    const text = await file.text();
    setJsonText(text);
    setPreview(null);
    setResult(null);
    setErrorMsg('');
    e.target.value = '';
  }

  async function handlePreview() {
    setLoadingPreview(true);
    setErrorMsg('');
    setResult(null);
    try {
      const parsed = normalizeBulkUsers(JSON.parse(jsonText));
      const { data: users, error: usersError } = await apiClient.GET('/users');

      if (usersError || !users) {
        throw new Error('admin.bulkParticipantsError');
      }

      const allUsersMap = new Map<string, UserPreviewItem>();
      for (const user of users) {
        allUsersMap.set(user.username.toLowerCase(), {
          id: user.id,
          username: user.username,
          display_name: user.display_name,
          roles: user.roles,
          created_at: user.created_at,
        });
      }

      const enrolledMap = new Map<string, boolean>();
      for (const participant of participants) {
        enrolledMap.set(participant.username.toLowerCase(), true);
      }

      const willCreate: ParsedBulkUser[] = [];
      const willAdd: UserPreviewItem[] = [];
      const alreadyEnrolled: UserPreviewItem[] = [];
      const existingWithPassword: UserPreviewItem[] = [];

      for (const item of parsed) {
        const key = item.username.toLowerCase();
        const isEnrolled = enrolledMap.has(key);
        const existing = allUsersMap.get(key);

        if (isEnrolled && existing) {
          if (item.password) {
            existingWithPassword.push(existing);
            continue;
          }
          alreadyEnrolled.push(existing);
          continue;
        }

        if (existing) {
          if (item.password) {
            existingWithPassword.push(existing);
            continue;
          }
          willAdd.push(existing);
        } else {
          willCreate.push(item);
        }
      }

      setPreview({
        willCreate,
        willAdd,
        alreadyEnrolled,
        existingWithPassword,
      });
      if (existingWithPassword.length > 0) {
        setErrorMsg(t('admin.bulkParticipantsExistingWithPassword'));
      }
    } catch (error) {
      if (error instanceof Error) {
        switch (error.message) {
          case 'admin.bulkParticipantsError':
            setErrorMsg(t('admin.bulkParticipantsError'));
            break;
          case 'admin.bulkParticipantsInvalidJson':
            setErrorMsg(t('admin.bulkParticipantsInvalidJson'));
            break;
          case 'admin.bulkParticipantsInvalidUsername':
            setErrorMsg(t('admin.bulkParticipantsInvalidUsername'));
            break;
          case 'admin.bulkParticipantsDuplicate':
            setErrorMsg(t('admin.bulkParticipantsDuplicate'));
            break;
          case 'admin.bulkParticipantsEmpty':
            setErrorMsg(t('admin.bulkParticipantsEmpty'));
            break;
          case 'admin.bulkParticipantsInvalidPassword':
            setErrorMsg(t('admin.bulkParticipantsInvalidPassword'));
            break;
          case 'admin.bulkParticipantsInvalidDisplayName':
            setErrorMsg(t('admin.bulkParticipantsInvalidDisplayName'));
            break;
          default:
            setErrorMsg(t('admin.bulkParticipantsInvalidJson'));
            break;
        }
      } else {
        setErrorMsg(t('admin.bulkParticipantsInvalidJson'));
      }
      setPreview(null);
    } finally {
      setLoadingPreview(false);
    }
  }

  async function handleConfirm() {
    if (!preview || preview.willCreate.length + preview.willAdd.length === 0)
      return;
    if (preview.existingWithPassword.length > 0) {
      setErrorMsg(t('admin.bulkParticipantsExistingWithPassword'));
      return;
    }

    setSubmitting(true);
    setErrorMsg('');
    const { data, error } = await apiClient.POST(
      '/contests/{id}/participants/bulk',
      {
        headers: { 'Idempotency-Key': getKey() },
        params: { path: { id: contest.id } },
        body: {
          usernames: preview.willAdd.map((user) => user.username),
          create_users: preview.willCreate.map((entry) => ({
            username: entry.username,
            display_name: entry.display_name,
            password: entry.password,
          })),
        },
      },
    );
    setSubmitting(false);

    if (error || !data) {
      const msg = getErrorMessage(error, t('admin.bulkParticipantsError'));
      setErrorMsg(msg);
      toast.error(msg);
      return;
    }

    resetKey();
    setResult(data);
    toast.success(t('toast.participant.bulkSuccess'));
    queryClient.invalidateQueries({
      queryKey: ['contest-participants', contest.id],
    });
    queryClient.invalidateQueries({ queryKey: ['admin-users'] });
    queryClient.invalidateQueries({ queryKey: ['all-users-for-participants'] });
  }

  return (
    <div className="space-y-3">
      <div className="space-y-3">
        <Label htmlFor="bulk-participants-json">
          {t('admin.bulkParticipantsJsonLabel')}
        </Label>
        <Input
          id="bulk-participants-json"
          type="file"
          accept="application/json"
          onChange={handleReadJsonFile}
        />
        <Textarea
          value={jsonText}
          onChange={(e) => {
            setJsonText(e.target.value);
            setPreview(null);
            setResult(null);
            setErrorMsg('');
          }}
          rows={6}
          placeholder={t('admin.bulkParticipantsJsonPlaceholder')}
        />
        <Button
          variant="outline"
          onClick={handlePreview}
          disabled={loadingPreview || !jsonText.trim()}
        >
          <Upload className="h-4 w-4 mr-1" />
          {loadingPreview
            ? t('admin.loading')
            : t('admin.bulkParticipantsPreview')}
        </Button>
      </div>

      {errorMsg && (
        <div className="rounded-md border border-destructive/20 bg-destructive/10 px-4 py-3 text-sm text-destructive">
          {errorMsg}
        </div>
      )}

      {preview && (
        <div className="space-y-3 rounded-md border p-4">
          <Label className="text-sm font-medium">
            {t('admin.bulkParticipantsPreviewTitle')}
          </Label>
          <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
            <div className="rounded-md border p-3">
              <p className="text-xs text-muted-foreground mb-1">
                {t('admin.bulkParticipantsWillCreate')}
              </p>
              <p className="text-lg font-semibold">
                {preview.willCreate.length}
              </p>
            </div>
            <div className="rounded-md border p-3">
              <p className="text-xs text-muted-foreground mb-1">
                {t('admin.bulkParticipantsWillAdd')}
              </p>
              <p className="text-lg font-semibold">{preview.willAdd.length}</p>
            </div>
            <div className="rounded-md border p-3">
              <p className="text-xs text-muted-foreground mb-1">
                {t('admin.bulkParticipantsAlreadyEnrolled')}
              </p>
              <p className="text-lg font-semibold">
                {preview.alreadyEnrolled.length}
              </p>
            </div>
            <div className="rounded-md border p-3">
              <p className="text-xs text-muted-foreground mb-1">
                {t('admin.bulkParticipantsExistingWithPasswordList')}
              </p>
              <p
                className={`text-lg font-semibold ${preview.existingWithPassword.length > 0 ? 'text-destructive' : ''}`}
              >
                {preview.existingWithPassword.length}
              </p>
            </div>
          </div>

          {preview.willCreate.length > 0 && (
            <div className="rounded-md border overflow-hidden">
              <div className="px-3 py-2 border-b bg-muted/40 text-sm font-medium">
                {t('admin.bulkParticipantsWillCreateList')}
              </div>
              <table className="w-full text-sm">
                <thead>
                  <tr className="border-b bg-muted/20">
                    <th className="px-3 py-2 text-left font-medium text-foreground/80">
                      {t('auth.username')}
                    </th>
                    <th className="px-3 py-2 text-left font-medium text-foreground/80">
                      {t('auth.displayName')}
                    </th>
                    <th className="px-3 py-2 text-left font-medium text-foreground/80">
                      {t('admin.field.password')}
                    </th>
                  </tr>
                </thead>
                <tbody>
                  {preview.willCreate.map((entry) => (
                    <tr
                      key={`create-${entry.username}`}
                      className="border-b last:border-0"
                    >
                      <td className="px-3 py-2">{entry.username}</td>
                      <td className="px-3 py-2">{entry.display_name}</td>
                      <td className="px-3 py-2 text-xs text-muted-foreground">
                        {entry.password ??
                          t('admin.bulkParticipantsAutoPassword')}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}

          <PreviewUserTable
            title={t('admin.bulkParticipantsWillAddList')}
            users={preview.willAdd}
          />
          <PreviewUserTable
            title={t('admin.bulkParticipantsAlreadyEnrolledList')}
            users={preview.alreadyEnrolled}
          />
          <PreviewUserTable
            title={t('admin.bulkParticipantsExistingWithPasswordList')}
            users={preview.existingWithPassword}
          />

          <DialogFooter>
            <Button
              onClick={handleConfirm}
              disabled={
                submitting ||
                preview.willCreate.length + preview.willAdd.length === 0 ||
                preview.existingWithPassword.length > 0
              }
            >
              {submitting
                ? t('admin.saving')
                : t('admin.bulkParticipantsConfirm')}
            </Button>
          </DialogFooter>
        </div>
      )}

      {result && (
        <div className="space-y-3 rounded-md border p-4">
          <Label className="text-sm font-medium">
            {t('admin.bulkParticipantsResultTitle')}
          </Label>
          <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
            <div className="rounded-md border p-3">
              <p className="text-xs text-muted-foreground mb-1">
                {t('admin.bulkParticipantsCreated')}
              </p>
              <p className="text-lg font-semibold">{result.created.length}</p>
            </div>
            <div className="rounded-md border p-3">
              <p className="text-xs text-muted-foreground mb-1">
                {t('admin.bulkParticipantsAdded')}
              </p>
              <p className="text-lg font-semibold">{result.added.length}</p>
            </div>
            <div className="rounded-md border p-3">
              <p className="text-xs text-muted-foreground mb-1">
                {t('admin.bulkParticipantsSkipped')}
              </p>
              <p className="text-lg font-semibold">
                {result.already_enrolled.length}
              </p>
            </div>
            <div className="rounded-md border p-3">
              <p className="text-xs text-muted-foreground mb-1">
                {t('admin.bulkParticipantsNotFound')}
              </p>
              <p className="text-lg font-semibold">{result.not_found.length}</p>
            </div>
          </div>

          {result.created.length > 0 && (
            <div className="rounded-md border overflow-hidden">
              <div className="p-2">
                <Button
                  variant="outline"
                  onClick={() =>
                    downloadUserCredentials(
                      result.created,
                      `contest-${contest.id}-user-credentials.json`,
                    )
                  }
                >
                  <Download className="mr-2 h-4 w-4" />
                  {t('users.create.download')}
                </Button>
              </div>
              <table className="w-full text-sm">
                <thead>
                  <tr className="border-b bg-muted/40">
                    <th className="px-3 py-2 text-left font-medium text-foreground/80">
                      {t('auth.username')}
                    </th>
                    <th className="px-3 py-2 text-left font-medium text-foreground/80">
                      {t('auth.displayName')}
                    </th>
                    <th className="px-3 py-2 text-left font-medium text-foreground/80">
                      {t('admin.field.password')}
                    </th>
                  </tr>
                </thead>
                <tbody>
                  {result.created.map((entry) => (
                    <tr key={entry.user_id} className="border-b last:border-0">
                      <td className="px-3 py-2">{entry.username}</td>
                      <td className="px-3 py-2">{entry.display_name}</td>
                      <td className="px-3 py-2 font-mono text-xs">
                        {entry.password}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}

          {result.added.length > 0 && (
            <div className="rounded-md border overflow-hidden">
              <div className="px-3 py-2 border-b bg-muted/40 text-sm font-medium">
                {t('admin.bulkParticipantsAddedList')}
              </div>
              <table className="w-full text-sm">
                <thead>
                  <tr className="border-b bg-muted/20">
                    <th className="px-3 py-2 text-left font-medium text-foreground/80">
                      #
                    </th>
                    <th className="px-3 py-2 text-left font-medium text-foreground/80">
                      {t('auth.username')}
                    </th>
                    <th className="px-3 py-2 text-left font-medium text-foreground/80">
                      {t('auth.displayName')}
                    </th>
                  </tr>
                </thead>
                <tbody>
                  {result.added.map((entry) => (
                    <tr key={entry.user_id} className="border-b last:border-0">
                      <td className="px-3 py-2">{entry.user_id}</td>
                      <td className="px-3 py-2">{entry.username}</td>
                      <td className="px-3 py-2">{entry.display_name}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

// -- Main Dialog --

export function ManageParticipantsDialog({
  contest,
  open,
  onOpenChange,
}: {
  contest: ContestSummary;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const { t } = useTranslation();
  const apiClient = useApiClient();
  const [activeTab, setActiveTab] = useState('enrolled');

  const { data: participants = [], isLoading } = useQuery({
    queryKey: ['contest-participants', contest.id],
    queryFn: () => fetchParticipants(apiClient, contest.id),
    enabled: open,
  });

  useEffect(() => {
    if (open) {
      setActiveTab('enrolled');
    }
  }, [open]);

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-3xl max-h-[90vh] flex flex-col">
        <DialogHeader>
          <DialogTitle>{t('admin.participants.title')}</DialogTitle>
          <DialogDescription>
            {t('admin.participants.description', { contest: contest.title })}
          </DialogDescription>
        </DialogHeader>

        <Tabs
          value={activeTab}
          onValueChange={setActiveTab}
          className="flex-1 min-h-0 flex flex-col"
        >
          <TabsList className="grid w-full grid-cols-3">
            <TabsTrigger value="enrolled" className="text-xs sm:text-sm">
              <Users className="h-3.5 w-3.5 mr-1.5" />
              {t('admin.participants.tabEnrolled')}
            </TabsTrigger>
            <TabsTrigger value="add" className="text-xs sm:text-sm">
              <UserPlus className="h-3.5 w-3.5 mr-1.5" />
              {t('admin.participants.tabAdd')}
            </TabsTrigger>
            <TabsTrigger value="bulk" className="text-xs sm:text-sm">
              <Upload className="h-3.5 w-3.5 mr-1.5" />
              {t('admin.participants.tabBulk')}
            </TabsTrigger>
          </TabsList>

          <TabsContent value="enrolled" className="flex-1 min-h-0 mt-4">
            <EnrolledTab
              contest={contest}
              participants={participants}
              isLoading={isLoading}
            />
          </TabsContent>

          <TabsContent value="add" className="flex-1 min-h-0 mt-4">
            <AddParticipantsTab contest={contest} participants={participants} />
          </TabsContent>

          <TabsContent
            value="bulk"
            className="flex-1 min-h-0 mt-4 overflow-y-auto px-0.5"
          >
            <BulkImportTab contest={contest} participants={participants} />
          </TabsContent>
        </Tabs>
      </DialogContent>
    </Dialog>
  );
}
