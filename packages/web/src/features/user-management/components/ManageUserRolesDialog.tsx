import { getErrorMessage, useApiClient } from '@broccoli/web-sdk/api';
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
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  useConfirmDialog,
} from '@broccoli/web-sdk/ui';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { Loader2, Plus, Trash2 } from 'lucide-react';
import { useEffect, useMemo, useState } from 'react';
import { toast } from 'sonner';

import type { ManagedUserRow } from '@/features/user-management/types';

interface ManageUserRolesDialogProps {
  user?: ManagedUserRow;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

export function ManageUserRolesDialog({
  user,
  open,
  onOpenChange,
}: ManageUserRolesDialogProps) {
  const { t } = useTranslation();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();

  const userId = user?.id;
  const [selectedRole, setSelectedRole] = useState('');
  const [manualRole, setManualRole] = useState('');
  const [isAssigning, setIsAssigning] = useState(false);
  const [confirm, confirmDialog] = useConfirmDialog();

  const { data: userDetail, isLoading: isLoadingUser } = useQuery({
    queryKey: ['admin-user-detail', userId],
    enabled: open && !!userId,
    queryFn: async () => {
      const { data, error } = await apiClient.GET('/users/{id}', {
        params: { path: { id: userId! } },
      });
      if (error) throw error;
      return {
        id: data.id,
        username: data.username,
        roles: data.roles,
      };
    },
  });

  const { data: roleOptions = [] } = useQuery({
    queryKey: ['admin-role-options'],
    enabled: open,
    staleTime: 60_000,
    queryFn: async () => {
      const { data, error } = await apiClient.GET('/roles');
      if (error) {
        return [];
      }
      return data;
    },
  });

  const currentRoles = useMemo(
    () => userDetail?.roles ?? user?.roles ?? [],
    [user?.roles, userDetail?.roles],
  );

  const assignableRoles = useMemo(() => {
    return roleOptions.filter((roleName) => !currentRoles.includes(roleName));
  }, [currentRoles, roleOptions]);

  useEffect(() => {
    if (!open) return;
    setManualRole('');
    setSelectedRole(assignableRoles[0] ?? '');
  }, [open, assignableRoles]);

  async function handleAssignRole() {
    if (!userId) return;

    const roleName = roleOptions.length > 0 ? selectedRole : manualRole.trim();

    if (!roleName) {
      toast.error(t('users.users.roleRequired'));
      return;
    }

    if (currentRoles.includes(roleName)) {
      toast.error(t('users.users.roleAlreadyAssigned'));
      return;
    }

    setIsAssigning(true);
    const { error } = await apiClient.POST('/users/{id}/roles', {
      params: { path: { id: userId } },
      body: { role: roleName },
    });
    setIsAssigning(false);

    if (error) {
      toast.error(getErrorMessage(error, t('users.users.assignRoleError')));
      return;
    }

    setManualRole('');
    toast.success(t('users.users.assignRoleSuccess'));
    queryClient.invalidateQueries({ queryKey: ['admin-users'] });
    queryClient.invalidateQueries({ queryKey: ['admin-user-detail', userId] });
  }

  function handleRevokeRole(roleName: string) {
    if (!userId) return;
    confirm({
      title: t('users.users.revokeRoleConfirm', { role: roleName }),
      confirmLabel: t('users.revoke'),
      destructive: true,
      failureMessage: t('users.users.revokeRoleError'),
      onConfirm: async () => {
        const { error } = await apiClient.DELETE('/users/{id}/roles/{role}', {
          params: { path: { id: userId, role: roleName } },
        });
        if (error) throw error;
        toast.success(t('users.users.revokeRoleSuccess'));
        queryClient.invalidateQueries({ queryKey: ['admin-users'] });
        queryClient.invalidateQueries({
          queryKey: ['admin-user-detail', userId],
        });
      },
    });
  }

  const canAssignFromSelect = roleOptions.length > 0 && !!selectedRole;
  const canAssignFromInput = roleOptions.length === 0 && !!manualRole.trim();
  const canAssign = (canAssignFromSelect || canAssignFromInput) && !isAssigning;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-xl">
        <DialogHeader>
          <DialogTitle>{t('users.users.manageRoles')}</DialogTitle>
          <DialogDescription>
            {userDetail
              ? t('users.users.manageRolesDescription', {
                  username: userDetail.username,
                })
              : t('users.users.manageRolesDescriptionFallback')}
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-4">
          <div className="rounded-md border max-h-64 overflow-y-auto">
            {isLoadingUser ? (
              <div className="p-4 text-sm text-muted-foreground">
                {t('admin.loading')}
              </div>
            ) : currentRoles.length === 0 ? (
              <div className="p-4 text-sm text-muted-foreground">
                {t('users.users.noRoles')}
              </div>
            ) : (
              <div className="divide-y">
                {currentRoles.map((roleName) => (
                  <div
                    key={roleName}
                    className="flex items-center justify-between gap-3 p-3"
                  >
                    <Badge variant="secondary">{roleName}</Badge>
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      className="text-destructive hover:text-destructive"
                      disabled={isAssigning || isLoadingUser}
                      onClick={() => handleRevokeRole(roleName)}
                    >
                      <Trash2 className="h-4 w-4" />
                    </Button>
                  </div>
                ))}
              </div>
            )}
          </div>

          <div className="space-y-2">
            <p className="text-sm font-medium">{t('users.users.assignRole')}</p>
            <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
              {roleOptions.length > 0 ? (
                <Select value={selectedRole} onValueChange={setSelectedRole}>
                  <SelectTrigger className="w-full">
                    <SelectValue
                      placeholder={t('users.users.roleSelectPlaceholder')}
                    />
                  </SelectTrigger>
                  <SelectContent>
                    {assignableRoles.length === 0 ? (
                      <SelectItem value="__none" disabled>
                        {t('users.users.noAssignableRoles')}
                      </SelectItem>
                    ) : (
                      assignableRoles.map((roleName) => (
                        <SelectItem key={roleName} value={roleName}>
                          {roleName}
                        </SelectItem>
                      ))
                    )}
                  </SelectContent>
                </Select>
              ) : (
                <Input
                  value={manualRole}
                  onChange={(e) => setManualRole(e.target.value)}
                  placeholder={t('users.users.roleInputPlaceholder')}
                />
              )}
              <Button
                type="button"
                onClick={handleAssignRole}
                disabled={!canAssign}
              >
                {isAssigning ? (
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                ) : (
                  <Plus className="mr-2 h-4 w-4" />
                )}
                {t('users.users.assignRoleButton')}
              </Button>
            </div>
          </div>
        </div>

        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => onOpenChange(false)}
          >
            {t('users.common.close')}
          </Button>
        </DialogFooter>
        {confirmDialog}
      </DialogContent>
    </Dialog>
  );
}
