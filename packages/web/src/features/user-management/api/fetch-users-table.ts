import type { ApiClient } from '@broccoli/web-sdk/api';
import type {
  ServerTableParams,
  ServerTableResponse,
} from '@broccoli/web-sdk/hooks';

import type { ManagedUserRow } from '@/features/user-management/types';

function compareStrings(a: string, b: string) {
  return a.localeCompare(b, undefined, { sensitivity: 'base' });
}

function compareNumbers(a: number, b: number) {
  return a - b;
}

function compareDates(a: string, b: string) {
  return new Date(a).getTime() - new Date(b).getTime();
}

function sortUsers(
  users: ManagedUserRow[],
  sortBy: string | undefined,
  sortOrder: 'asc' | 'desc' | undefined,
) {
  const direction = sortOrder === 'asc' ? 1 : -1;

  return [...users].sort((left, right) => {
    let result = 0;

    if (sortBy === 'id') {
      result = compareNumbers(left.id, right.id);
    } else if (sortBy === 'username') {
      result = compareStrings(left.username, right.username);
    } else if (sortBy === 'display_name') {
      result = compareStrings(left.display_name, right.display_name);
    } else if (sortBy === 'roles') {
      result = compareStrings(left.roles.join(', '), right.roles.join(', '));
    } else {
      result = compareDates(left.created_at, right.created_at);
    }

    if (result === 0) {
      result = compareNumbers(left.id, right.id);
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

export async function fetchUsersTable(
  apiClient: ApiClient,
  params: ServerTableParams,
): Promise<ServerTableResponse<ManagedUserRow>> {
  const { data, error } = await apiClient.GET('/users');
  if (error) throw error;

  const users: ManagedUserRow[] = data.map((item) => ({
    id: item.id,
    username: item.username,
    display_name: item.display_name,
    roles: item.roles,
    created_at: item.created_at,
  }));

  const keyword = params.search?.trim().toLowerCase();
  const filtered = keyword
    ? users.filter((user) => {
        if (user.username.toLowerCase().includes(keyword)) return true;
        if (user.display_name.toLowerCase().includes(keyword)) return true;
        if (String(user.id).includes(keyword)) return true;
        return user.roles.some((role) => role.toLowerCase().includes(keyword));
      })
    : users;

  const sorted = sortUsers(filtered, params.sort_by, params.sort_order);
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
