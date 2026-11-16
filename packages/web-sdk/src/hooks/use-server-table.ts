import { keepPreviousData, useQuery } from '@tanstack/react-query';
import { useCallback, useEffect, useRef, useState } from 'react';

import { type ApiClient, useApiClient } from '@/api';
import { queryState } from '@/hooks/query-state';

export interface ServerTableParams {
  page: number;
  per_page: number;
  search?: string;
  sort_by?: string;
  sort_order?: 'asc' | 'desc';
}

export interface PaginationMeta {
  page: number;
  per_page: number;
  total: number;
  total_pages: number;
}

export interface ServerTableResponse<T> {
  data: T[];
  pagination: PaginationMeta;
}

export interface ControlledServerTableState {
  page: number;
  search: string;
  sortBy?: string;
  sortOrder: 'asc' | 'desc';
}

export interface UseServerTableOptions<T> {
  queryKey: string[];
  fetchFn: (
    apiClient: ApiClient,
    params: ServerTableParams,
  ) => Promise<ServerTableResponse<T>>;
  defaultPerPage?: number;
  defaultSortBy?: string;
  defaultSortOrder?: 'asc' | 'desc';
  debounceMs?: number;
  state?: ControlledServerTableState;
  onPageChange?: (page: number) => void;
  onSearchChange?: (search: string) => void;
  onSortChange?: (
    sortBy: string | undefined,
    sortOrder: 'asc' | 'desc',
  ) => void;
}

export function useServerTable<T>({
  queryKey,
  fetchFn,
  defaultPerPage = 20,
  defaultSortBy,
  defaultSortOrder = 'desc',
  debounceMs = 300,
  state,
  onPageChange,
  onSearchChange,
  onSortChange,
}: UseServerTableOptions<T>) {
  const [internalPage, setInternalPage] = useState(1);
  const [perPage, setPerPage] = useState(defaultPerPage);
  const [internalSearch, setInternalSearch] = useState('');
  const [debouncedSearch, setDebouncedSearch] = useState(
    () => state?.search ?? '',
  );
  const [internalSortBy, setInternalSortBy] = useState(defaultSortBy);
  const [internalSortOrder, setInternalSortOrder] = useState<'asc' | 'desc'>(
    defaultSortOrder,
  );
  const debounceTimer = useRef<ReturnType<typeof setTimeout>>(null);
  const apiClient = useApiClient();
  const page = state?.page ?? internalPage;
  const search = state?.search ?? internalSearch;
  const sortBy = state ? state.sortBy : internalSortBy;
  const sortOrder = state?.sortOrder ?? internalSortOrder;

  useEffect(() => {
    debounceTimer.current = setTimeout(() => {
      setDebouncedSearch(search);
    }, debounceMs);
    return () => {
      if (debounceTimer.current) clearTimeout(debounceTimer.current);
    };
  }, [search, debounceMs]);

  const params: ServerTableParams = {
    page,
    per_page: perPage,
    ...(debouncedSearch && { search: debouncedSearch }),
    ...(sortBy && { sort_by: sortBy }),
    ...(sortBy && { sort_order: sortOrder }),
  };

  const query = useQuery({
    queryKey: [...queryKey, params],
    queryFn: () => fetchFn(apiClient, params),
    placeholderData: keepPreviousData,
  });
  const { data, isLoading, isFetching } = query;

  const setPage = useCallback(
    (nextPage: number) => {
      if (state) {
        onPageChange?.(nextPage);
        return;
      }
      setInternalPage(nextPage);
    },
    [onPageChange, state],
  );

  const setSearch = useCallback(
    (nextSearch: string) => {
      if (state) {
        onSearchChange?.(nextSearch);
        return;
      }
      setInternalSearch(nextSearch);
      setInternalPage(1);
    },
    [onSearchChange, state],
  );

  const toggleSort = useCallback(
    (column: string) => {
      const nextSortOrder =
        sortBy === column ? (sortOrder === 'asc' ? 'desc' : 'asc') : 'asc';

      if (state) {
        onSortChange?.(column, nextSortOrder);
        return;
      }

      if (sortBy === column) {
        setInternalSortOrder((prev) => (prev === 'asc' ? 'desc' : 'asc'));
      } else {
        setInternalSortBy(column);
        setInternalSortOrder('asc');
      }
      setInternalPage(1);
    },
    [onSortChange, sortBy, sortOrder, state],
  );

  const pagination = data?.pagination ?? {
    page: 1,
    per_page: perPage,
    total: 0,
    total_pages: 0,
  };

  return {
    /**
     * The page's rows; empty until they arrive. Render through `view`, which
     * tells an empty page from one that failed or has not loaded.
     */
    data: data?.data ?? [],
    /** The page as a `QueryState`: loading, failed, empty or rows. */
    view: queryState(
      { ...query, data: data?.data },
      (rows) => rows.length === 0,
    ),
    pagination,
    isLoading,
    isFetching,
    page,
    perPage,
    search,
    sortBy,
    sortOrder,
    setPage,
    setPerPage: (v: number) => {
      setPerPage(v);
      if (state) {
        onPageChange?.(1);
        return;
      }
      setInternalPage(1);
    },
    setSearch,
    toggleSort,
  };
}
