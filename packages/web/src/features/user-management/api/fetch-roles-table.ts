import type { ApiClient } from '@broccoli/web-sdk/api';
import type {
  ServerTableParams,
  ServerTableResponse,
} from '@broccoli/web-sdk/hooks';

import type { RolePermissionsRow } from '@/features/user-management/types';

function compareStrings(a: string, b: string) {
  return a.localeCompare(b, undefined, { sensitivity: 'base' });
}

function compareNumbers(a: number, b: number) {
  return a - b;
}

function sortRoles(
  rows: RolePermissionsRow[],
  sortBy: string | undefined,
  sortOrder: 'asc' | 'desc' | undefined,
) {
  const direction = sortOrder === 'asc' ? 1 : -1;

  return [...rows].sort((left, right) => {
    let result = 0;

    if (sortBy === 'permission_count') {
      result = compareNumbers(left.permission_count, right.permission_count);
    } else if (sortBy === 'permissions') {
      result = compareStrings(
        left.permissions.join('\n'),
        right.permissions.join('\n'),
      );
    } else {
      result = compareStrings(left.role, right.role);
    }

    if (result === 0) {
      result = compareStrings(left.role, right.role);
    }

    return result * direction;
  });
}

function paginateRows<T>(rows: T[], page: number, perPage: number) {
  const safePerPage = Math.max(1, perPage);
  const total = rows.length;
  const totalPages = Math.max(1, Math.ceil(total / safePerPage));
  const safePage = Math.min(Math.max(1, page), totalPages);
  const start = (safePage - 1) * safePerPage;
  const end = start + safePerPage;

  return {
    pageRows: rows.slice(start, end),
    pagination: {
      page: safePage,
      per_page: safePerPage,
      total,
      total_pages: totalPages,
    },
  };
}

export async function fetchRolesTable(
  apiClient: ApiClient,
  params: ServerTableParams,
): Promise<ServerTableResponse<RolePermissionsRow>> {
  const { data: roles, error } = await apiClient.GET('/roles');
  if (error) throw error;

  const rows = await Promise.all(
    roles.map(async (role) => {
      const { data: permissions, error: permissionsError } =
        await apiClient.GET('/roles/{role}/permissions', {
          params: { path: { role } },
        });

      if (permissionsError) throw permissionsError;

      const sortedPermissions = [...permissions].sort((a, b) =>
        compareStrings(a, b),
      );

      return {
        role,
        permissions: sortedPermissions,
        permission_count: sortedPermissions.length,
      } satisfies RolePermissionsRow;
    }),
  );

  const keyword = params.search?.trim().toLowerCase();
  const filtered = keyword
    ? rows.filter((row) => {
        if (row.role.toLowerCase().includes(keyword)) return true;
        return row.permissions.some((permission) =>
          permission.toLowerCase().includes(keyword),
        );
      })
    : rows;

  const sorted = sortRoles(filtered, params.sort_by, params.sort_order);
  const { pageRows, pagination } = paginateRows(
    sorted,
    params.page,
    params.per_page,
  );

  return {
    data: pageRows,
    pagination,
  };
}
