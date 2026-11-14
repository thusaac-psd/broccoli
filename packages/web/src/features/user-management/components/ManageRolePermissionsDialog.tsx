import { useApiClient, useErrorMessage } from '@broccoli/web-sdk/api';
import { queryState } from '@broccoli/web-sdk/hooks';
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
  QueryErrorState,
  useConfirmDialog,
} from '@broccoli/web-sdk/ui';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { Loader2, Plus, Trash2 } from 'lucide-react';
import { useEffect, useState } from 'react';
import { toast } from 'sonner';

import type { RolePermissionsRow } from '@/features/user-management/types';

const NO_PERMISSIONS: string[] = [];

interface ManageRolePermissionsDialogProps {
  role?: RolePermissionsRow;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

export function ManageRolePermissionsDialog({
  role,
  open,
  onOpenChange,
}: ManageRolePermissionsDialogProps) {
  const { t } = useTranslation();
  const errorMessage = useErrorMessage();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();

  const roleName = role?.role;
  const [newPermission, setNewPermission] = useState('');
  const [isGranting, setIsGranting] = useState(false);
  const [confirm, confirmDialog] = useConfirmDialog();

  useEffect(() => {
    if (open) {
      setNewPermission('');
    }
  }, [open, roleName]);

  const permissionsQuery = useQuery({
    queryKey: ['admin-role-permissions-detail', roleName],
    enabled: open && !!roleName,
    queryFn: async () => {
      const { data, error } = await apiClient.GET('/roles/{role}/permissions', {
        params: { path: { role: roleName! } },
      });
      if (error) throw error;
      return [...data].sort((a, b) => a.localeCompare(b));
    },
  });
  const { isFetching } = permissionsQuery;
  const permissionsState = queryState(permissionsQuery);
  const permissions = permissionsQuery.data ?? NO_PERMISSIONS;

  async function handleGrantPermission() {
    if (!roleName) return;

    const permission = newPermission.trim();
    if (!permission) {
      toast.error(t('users.roles.permissionRequired'));
      return;
    }

    if (permissions.includes(permission)) {
      toast.error(t('users.roles.permissionExists'));
      return;
    }

    setIsGranting(true);
    const { error } = await apiClient.POST('/roles/{role}/permissions', {
      params: { path: { role: roleName } },
      body: { permission },
    });
    setIsGranting(false);

    if (error) {
      toast.error(errorMessage(error, t('users.roles.grantPermissionError')));
      return;
    }

    setNewPermission('');
    toast.success(t('users.roles.grantPermissionSuccess'));
    queryClient.invalidateQueries({ queryKey: ['admin-role-permissions'] });
    queryClient.invalidateQueries({
      queryKey: ['admin-role-permissions-detail', roleName],
    });
  }

  function handleRevokePermission(permission: string) {
    if (!roleName) return;
    confirm({
      title: t('users.roles.revokePermissionConfirm', { permission }),
      confirmLabel: t('users.revoke'),
      destructive: true,
      failureMessage: t('users.roles.revokePermissionError'),
      onConfirm: async () => {
        const { error } = await apiClient.DELETE(
          '/roles/{role}/permissions/{permission}',
          { params: { path: { role: roleName, permission } } },
        );
        if (error) throw error;
        toast.success(t('users.roles.revokePermissionSuccess'));
        queryClient.invalidateQueries({ queryKey: ['admin-role-permissions'] });
        queryClient.invalidateQueries({
          queryKey: ['admin-role-permissions-detail', roleName],
        });
      },
    });
  }

  const normalizedPermission = newPermission.trim();
  const canGrant =
    !!normalizedPermission &&
    !permissions.includes(normalizedPermission) &&
    !isGranting;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-xl">
        <DialogHeader>
          <DialogTitle>{t('users.roles.managePermissions')}</DialogTitle>
          <DialogDescription>
            {roleName
              ? t('users.roles.managePermissionsFor', { role: roleName })
              : t('users.roles.managePermissionsDescription')}
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-4">
          <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
            <Input
              value={newPermission}
              onChange={(e) => setNewPermission(e.target.value)}
              placeholder={t('users.roles.permissionPlaceholder')}
            />
            <Button
              type="button"
              onClick={handleGrantPermission}
              disabled={!canGrant}
            >
              {isGranting ? (
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              ) : (
                <Plus className="mr-2 h-4 w-4" />
              )}
              {t('users.roles.addPermission')}
            </Button>
          </div>

          <div className="rounded-md border max-h-80 overflow-y-auto">
            {permissionsState.status === 'error' ? (
              <QueryErrorState
                className="py-6"
                error={permissionsState.error}
                retry={permissionsState.retry}
                retrying={permissionsState.retrying}
                failure={t('users.roles.permissionsLoadFailed')}
              />
            ) : permissionsState.status === 'loading' ? (
              <div className="p-4 text-sm text-muted-foreground">
                {t('admin.loading')}
              </div>
            ) : permissions.length === 0 ? (
              <div className="p-4 text-sm text-muted-foreground">
                {t('users.roles.noPermissions')}
              </div>
            ) : (
              <div className="divide-y">
                {permissions.map((permission) => (
                  <div
                    key={permission}
                    className="flex items-center justify-between gap-3 p-3"
                  >
                    <Badge variant="secondary" className="font-mono text-xs">
                      {permission}
                    </Badge>
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      className="text-destructive hover:text-destructive"
                      disabled={isFetching || isGranting}
                      onClick={() => handleRevokePermission(permission)}
                    >
                      <Trash2 className="h-4 w-4" />
                    </Button>
                  </div>
                ))}
              </div>
            )}
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
