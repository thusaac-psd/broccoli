import { useApiClient } from '@broccoli/web-sdk/api';
import { useAuth, useAuthReady } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { CONTEST_MANAGE } from '@broccoli/web-sdk/permissions';
import { Slot } from '@broccoli/web-sdk/slot';
import { Button } from '@broccoli/web-sdk/ui';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { Flame, List, Pencil, Trash2, Users } from 'lucide-react';
import { useState } from 'react';
import { useNavigate, useParams } from 'react-router';

import {
  ContestFormDialog,
  ContestProblemsDialog,
} from '@/features/admin/components/AdminContestsTab';
import { ManageParticipantsDialog } from '@/features/admin/components/ManageParticipantsDialog';
import { PrewarmDialog } from '@/features/contest/components/PrewarmDialog';

export function ContestAdminActions() {
  const { contestId } = useParams();
  const { t } = useTranslation();
  const { user } = useAuth();
  const authReady = useAuthReady();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();
  const navigate = useNavigate();

  const id = Number(contestId);

  const { data: contest } = useQuery({
    queryKey: ['contest', id],
    enabled: authReady && Number.isFinite(id) && id > 0,
    queryFn: async () => {
      const { data, error } = await apiClient.GET('/contests/{id}', {
        params: { path: { id } },
      });
      if (error) throw error;
      return data;
    },
    staleTime: 60_000,
  });

  const [editOpen, setEditOpen] = useState(false);
  const [problemsOpen, setProblemsOpen] = useState(false);
  const [participantsOpen, setParticipantsOpen] = useState(false);
  const [warmOpen, setWarmOpen] = useState(false);

  if (!user?.permissions?.includes(CONTEST_MANAGE) || !contest) return null;

  async function handleDelete() {
    if (!window.confirm(t('admin.deleteConfirm'))) return;
    const { error } = await apiClient.DELETE('/contests/{id}', {
      params: { path: { id } },
    });
    if (!error) {
      queryClient.invalidateQueries({ queryKey: ['admin-contests'] });
      navigate('/contests');
    }
  }

  return (
    <div className="rounded-lg border p-4 space-y-1">
      <p className="text-[10px] font-semibold uppercase tracking-[0.15em] text-muted-foreground mb-2">
        {t('admin.actions')}
      </p>
      <Slot name="contest.admin.actions" />
      <Button
        variant="ghost"
        size="sm"
        className="w-full justify-start gap-2 h-8 text-xs"
        onClick={() => setProblemsOpen(true)}
      >
        <List className="h-3.5 w-3.5" />
        {t('admin.contestProblems')}
      </Button>
      <Button
        variant="ghost"
        size="sm"
        className="w-full justify-start gap-2 h-8 text-xs"
        onClick={() => setParticipantsOpen(true)}
      >
        <Users className="h-3.5 w-3.5" />
        {t('admin.bulkParticipantsAction')}
      </Button>
      <Button
        variant="ghost"
        size="sm"
        className="w-full justify-start gap-2 h-8 text-xs"
        onClick={() => setWarmOpen(true)}
      >
        <Flame className="h-3.5 w-3.5" />
        {t('admin.warmCaches')}
      </Button>
      <Button
        variant="ghost"
        size="sm"
        className="w-full justify-start gap-2 h-8 text-xs"
        onClick={() => setEditOpen(true)}
      >
        <Pencil className="h-3.5 w-3.5" />
        {t('admin.edit')}
      </Button>
      <Button
        variant="ghost"
        size="sm"
        className="w-full justify-start gap-2 h-8 text-xs text-destructive hover:text-destructive"
        onClick={handleDelete}
      >
        <Trash2 className="h-3.5 w-3.5" />
        {t('admin.delete')}
      </Button>

      <ContestFormDialog
        contest={contest}
        open={editOpen}
        onOpenChange={setEditOpen}
      />
      <ContestProblemsDialog
        contest={contest}
        open={problemsOpen}
        onOpenChange={setProblemsOpen}
      />
      <ManageParticipantsDialog
        contest={contest}
        open={participantsOpen}
        onOpenChange={setParticipantsOpen}
      />
      <PrewarmDialog
        contestId={id}
        open={warmOpen}
        onOpenChange={setWarmOpen}
      />
    </div>
  );
}
