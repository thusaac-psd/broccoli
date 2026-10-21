import { getErrorMessage, useApiClient } from '@broccoli/web-sdk/api';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import {
  Badge,
  Button,
  DataTable,
  type DataTableColumn,
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@broccoli/web-sdk/ui';
import { formatDateTime } from '@broccoli/web-sdk/utils';
import { useQueryClient } from '@tanstack/react-query';
import {
  MoreHorizontal,
  Pencil,
  ShieldCheck,
  Trash2,
  UserPlus,
} from 'lucide-react';
import { useMemo, useState } from 'react';
import { toast } from 'sonner';

import { fetchUsersTable } from '@/features/user-management/api/fetch-users-table';
import { CreateUsersDialog } from '@/features/user-management/components/CreateUsersDialog';
import { EditUserDialog } from '@/features/user-management/components/EditUserDialog';
import { ManageUserRolesDialog } from '@/features/user-management/components/ManageUserRolesDialog';
import type { ManagedUserRow } from '@/features/user-management/types';
import { useTableSearchParams } from '@/hooks/use-table-search-params';

function useUserColumns({
  locale,
  onEdit,
  onManageRoles,
  onDelete,
}: {
  locale: string;
  onEdit: (user: ManagedUserRow) => void;
  onManageRoles: (user: ManagedUserRow) => void;
  onDelete: (user: ManagedUserRow) => void;
}): DataTableColumn<ManagedUserRow>[] {
  const { t } = useTranslation();

  return useMemo(
    () => [
      {
        accessorKey: 'id',
        header: '#',
        size: 72,
        sortKey: 'id',
      },
      {
        accessorKey: 'username',
        header: t('users.users.username'),
        sortKey: 'username',
        cell: ({ row }) => (
          <span className="font-medium text-foreground">
            {row.original.username}
          </span>
        ),
      },
      {
        accessorKey: 'display_name',
        header: t('auth.displayName'),
        sortKey: 'display_name',
        cell: ({ row }) => row.original.display_name,
      },
      {
        accessorKey: 'roles',
        header: t('users.users.roles'),
        sortKey: 'roles',
        cell: ({ row }) => {
          const roles = row.original.roles;
          if (roles.length === 0) {
            return (
              <span className="text-xs text-muted-foreground">
                {t('users.users.noRoles')}
              </span>
            );
          }

          return (
            <div className="flex flex-wrap gap-1">
              {roles.slice(0, 3).map((role) => (
                <Badge key={role} variant="outline">
                  {role}
                </Badge>
              ))}
              {roles.length > 3 && (
                <Badge variant="secondary">+{roles.length - 3}</Badge>
              )}
            </div>
          );
        },
      },
      {
        accessorKey: 'created_at',
        header: t('admin.field.createdAt'),
        sortKey: 'created_at',
        cell: ({ row }) => formatDateTime(row.original.created_at, locale),
      },
      {
        id: 'actions',
        header: '',
        size: 80,
        cell: ({ row }) => (
          <div className="flex justify-end">
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button variant="ghost" size="icon" className="h-7 w-7">
                  <MoreHorizontal className="h-4 w-4" />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                <DropdownMenuItem onClick={() => onManageRoles(row.original)}>
                  <ShieldCheck className="h-4 w-4" />
                  {t('users.users.manageRoles')}
                </DropdownMenuItem>
                <DropdownMenuItem onClick={() => onEdit(row.original)}>
                  <Pencil className="h-4 w-4" />
                  {t('users.users.editUser')}
                </DropdownMenuItem>
                <DropdownMenuSeparator />
                <DropdownMenuItem
                  className="text-destructive focus:text-destructive"
                  onClick={() => onDelete(row.original)}
                >
                  <Trash2 className="h-4 w-4" />
                  {t('admin.delete')}
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </div>
        ),
      },
    ],
    [locale, onDelete, onEdit, onManageRoles, t],
  );
}

export function UsersManagementTab() {
  const { t, locale } = useTranslation();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();
  const table = useTableSearchParams({
    defaultSortBy: 'created_at',
    defaultSortOrder: 'desc',
  });

  const [createDialogOpen, setCreateDialogOpen] = useState(false);
  const [editingUser, setEditingUser] = useState<ManagedUserRow>();
  const [editDialogOpen, setEditDialogOpen] = useState(false);
  const [roleUser, setRoleUser] = useState<ManagedUserRow>();
  const [rolesDialogOpen, setRolesDialogOpen] = useState(false);

  function handleEditUser(user: ManagedUserRow) {
    setEditingUser(user);
    setEditDialogOpen(true);
  }

  function handleManageRoles(user: ManagedUserRow) {
    setRoleUser(user);
    setRolesDialogOpen(true);
  }

  async function handleDeleteUser(user: ManagedUserRow) {
    if (
      !window.confirm(
        t('users.users.deleteUserConfirm', { username: user.username }),
      )
    ) {
      return;
    }

    const { error } = await apiClient.DELETE('/users/{id}', {
      params: { path: { id: user.id } },
    });

    if (error) {
      toast.error(getErrorMessage(error, t('users.users.deleteUserError')));
      return;
    }

    toast.success(t('users.users.deleteUserSuccess'));
    queryClient.invalidateQueries({ queryKey: ['admin-users'] });
  }

  const columns = useUserColumns({
    locale,
    onEdit: handleEditUser,
    onManageRoles: handleManageRoles,
    onDelete: handleDeleteUser,
  });

  return (
    <>
      <DataTable
        columns={columns}
        queryKey={['admin-users']}
        fetchFn={fetchUsersTable}
        searchable
        toolbar={
          <Button onClick={() => setCreateDialogOpen(true)}>
            <UserPlus className="mr-2 h-4 w-4" />
            {t('users.create.title')}
          </Button>
        }
        searchPlaceholder={t('users.users.searchPlaceholder')}
        defaultPerPage={20}
        defaultSortBy="created_at"
        defaultSortOrder="desc"
        emptyMessage={t('users.users.empty')}
        state={table.state}
        onPageChange={table.setPage}
        onSearchChange={table.setSearch}
        onSortChange={table.setSort}
      />

      {createDialogOpen && (
        <CreateUsersDialog onOpenChange={setCreateDialogOpen} />
      )}

      <EditUserDialog
        user={editingUser}
        open={editDialogOpen}
        onOpenChange={setEditDialogOpen}
      />

      <ManageUserRolesDialog
        user={roleUser}
        open={rolesDialogOpen}
        onOpenChange={setRolesDialogOpen}
      />
    </>
  );
}
