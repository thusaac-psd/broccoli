import {
  type ColumnDef,
  flexRender,
  getCoreRowModel,
  useReactTable,
} from '@tanstack/react-table';
import {
  ArrowDown,
  ArrowUp,
  ArrowUpDown,
  ChevronLeft,
  ChevronRight,
  ChevronsLeft,
  ChevronsRight,
  Search,
} from 'lucide-react';

import type { ApiClient } from '@/api';
import type {
  ControlledServerTableState,
  PaginationMeta,
  ServerTableResponse,
} from '@/hooks/use-server-table';
import { useServerTable } from '@/hooks/use-server-table';
import { useTranslation } from '@/i18n';
import { Button } from '@/ui/button';
import { Input } from '@/ui/input';
import { QueryErrorState } from '@/ui/query-view';
import { Skeleton } from '@/ui/skeleton';

// Re-export ColumnDef for consumers
export type { ColumnDef } from '@tanstack/react-table';

export type DataTableColumn<TData> = ColumnDef<TData, unknown> & {
  sortKey?: string;
};

export interface DataTableProps<TData> {
  columns: DataTableColumn<TData>[];
  queryKey: string[];
  fetchFn: (
    api: ApiClient,
    params: {
      page: number;
      per_page: number;
      search?: string;
      sort_by?: string;
      sort_order?: 'asc' | 'desc';
    },
  ) => Promise<ServerTableResponse<TData>>;
  searchable?: boolean;
  searchPlaceholder?: string;
  defaultPerPage?: number;
  defaultSortBy?: string;
  defaultSortOrder?: 'asc' | 'desc';
  emptyMessage?: string;
  toolbar?: React.ReactNode;
  state?: ControlledServerTableState;
  onPageChange?: (page: number) => void;
  onSearchChange?: (search: string) => void;
  onSortChange?: (
    sortBy: string | undefined,
    sortOrder: 'asc' | 'desc',
  ) => void;
}

function SortIcon({
  column,
  currentSort,
  currentOrder,
}: {
  column: string;
  currentSort?: string;
  currentOrder: 'asc' | 'desc';
}) {
  if (currentSort !== column) {
    return <ArrowUpDown className="ml-1 h-3 w-3 opacity-40" />;
  }
  return currentOrder === 'asc' ? (
    <ArrowUp className="ml-1 h-3 w-3" />
  ) : (
    <ArrowDown className="ml-1 h-3 w-3" />
  );
}

function PaginationControls({
  pagination,
  page,
  setPage,
  isFetching,
}: {
  pagination: PaginationMeta;
  page: number;
  setPage: (p: number) => void;
  isFetching: boolean;
}) {
  const { total, total_pages } = pagination;
  const { t } = useTranslation();
  const canPrev = page > 1;
  const canNext = page < total_pages;

  return (
    <div className="flex items-center justify-between px-4 py-3 border-t">
      <span className="text-xs text-muted-foreground">
        {t('table.results', { count: total })}
        {isFetching && ` · ${t('table.loading')}`}
      </span>
      <div className="flex items-center gap-1">
        <Button
          aria-label={t('table.firstPage')}
          variant="ghost"
          size="icon"
          className="h-7 w-7"
          onClick={() => setPage(1)}
          disabled={!canPrev}
        >
          <ChevronsLeft className="h-3.5 w-3.5" />
        </Button>
        <Button
          aria-label={t('table.previousPage')}
          variant="ghost"
          size="icon"
          className="h-7 w-7"
          onClick={() => setPage(page - 1)}
          disabled={!canPrev}
        >
          <ChevronLeft className="h-3.5 w-3.5" />
        </Button>
        <span className="text-xs px-2 text-muted-foreground">
          {page} / {total_pages || 1}
        </span>
        <Button
          aria-label={t('table.nextPage')}
          variant="ghost"
          size="icon"
          className="h-7 w-7"
          onClick={() => setPage(page + 1)}
          disabled={!canNext}
        >
          <ChevronRight className="h-3.5 w-3.5" />
        </Button>
        <Button
          aria-label={t('table.lastPage')}
          variant="ghost"
          size="icon"
          className="h-7 w-7"
          onClick={() => setPage(total_pages)}
          disabled={!canNext}
        >
          <ChevronsRight className="h-3.5 w-3.5" />
        </Button>
      </div>
    </div>
  );
}

export function DataTable<TData>({
  columns,
  queryKey,
  fetchFn,
  searchable = false,
  searchPlaceholder,
  defaultPerPage = 20,
  defaultSortBy,
  defaultSortOrder = 'desc',
  emptyMessage,
  toolbar,
  state,
  onPageChange,
  onSearchChange,
  onSortChange,
}: DataTableProps<TData>) {
  const { t } = useTranslation();
  const {
    data,
    view,
    pagination,
    isFetching,
    page,
    search,
    sortBy,
    sortOrder,
    setPage,
    setSearch,
    toggleSort,
  } = useServerTable<TData>({
    queryKey,
    fetchFn,
    defaultPerPage,
    defaultSortBy,
    defaultSortOrder,
    state,
    onPageChange,
    onSearchChange,
    onSortChange,
  });

  const table = useReactTable({
    data,
    columns,
    getCoreRowModel: getCoreRowModel(),
    manualPagination: true,
    manualSorting: true,
    rowCount: pagination.total,
  });

  return (
    <div className="rounded-md border">
      {(searchable || toolbar) && (
        <div className="flex items-center gap-3 p-4 border-b">
          {searchable && (
            <div className="relative flex-1 max-w-sm">
              <Search
                className="pointer-events-none absolute top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground"
                style={{ insetInlineStart: '0.625rem' }}
              />
              <Input
                placeholder={searchPlaceholder ?? t('table.search')}
                value={search}
                onChange={(e) => setSearch(e.target.value)}
                className="h-8 text-sm"
                style={{ paddingInlineStart: '2rem' }}
              />
            </div>
          )}
          {toolbar}
        </div>
      )}

      <div className="overflow-x-auto">
        <table className="w-full text-sm">
          <thead>
            {table.getHeaderGroups().map((headerGroup) => (
              <tr key={headerGroup.id} className="border-b bg-muted/40">
                {headerGroup.headers.map((header) => {
                  const colDef = header.column
                    .columnDef as DataTableColumn<TData>;
                  const isSortable = !!colDef.sortKey;

                  return (
                    <th
                      key={header.id}
                      className={`px-3 py-2.5 text-left font-medium text-foreground/80 ${
                        isSortable
                          ? 'cursor-pointer select-none hover:text-foreground transition-colors'
                          : ''
                      }`}
                      style={{
                        width:
                          header.getSize() !== 150
                            ? header.getSize()
                            : undefined,
                      }}
                      onClick={
                        isSortable
                          ? () => toggleSort(colDef.sortKey!)
                          : undefined
                      }
                    >
                      <div className="flex items-center">
                        {header.isPlaceholder
                          ? null
                          : flexRender(
                              header.column.columnDef.header,
                              header.getContext(),
                            )}
                        {isSortable && (
                          <SortIcon
                            column={colDef.sortKey!}
                            currentSort={sortBy}
                            currentOrder={sortOrder}
                          />
                        )}
                      </div>
                    </th>
                  );
                })}
              </tr>
            ))}
          </thead>
          <tbody>
            {view.status === 'loading' || view.status === 'idle' ? (
              Array.from({ length: 5 }).map((_, i) => (
                <tr key={i} className="border-b">
                  {columns.map((_, j) => (
                    <td key={j} className="px-3 py-2.5">
                      <Skeleton className="h-4 w-full" />
                    </td>
                  ))}
                </tr>
              ))
            ) : view.status === 'error' ? (
              <tr>
                <td colSpan={columns.length}>
                  <QueryErrorState
                    error={view.error}
                    retry={view.retry}
                    retrying={view.retrying}
                  />
                </td>
              </tr>
            ) : table.getRowModel().rows.length === 0 ? (
              <tr>
                <td
                  colSpan={columns.length}
                  className="px-3 py-12 text-center text-muted-foreground"
                >
                  {emptyMessage ?? t('table.noResults')}
                </td>
              </tr>
            ) : (
              table.getRowModel().rows.map((row) => (
                <tr
                  key={row.id}
                  className="border-b last:border-0 transition-colors hover:bg-muted/30"
                >
                  {row.getVisibleCells().map((cell) => (
                    <td key={cell.id} className="px-3 py-2.5">
                      {flexRender(
                        cell.column.columnDef.cell,
                        cell.getContext(),
                      )}
                    </td>
                  ))}
                </tr>
              ))
            )}
          </tbody>
        </table>
      </div>

      <PaginationControls
        pagination={pagination}
        page={page}
        setPage={setPage}
        isFetching={isFetching}
      />
    </div>
  );
}
