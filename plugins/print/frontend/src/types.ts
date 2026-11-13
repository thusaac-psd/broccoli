export type PrintStatus =
  | 'pending_approval'
  | 'pending'
  | 'claimed'
  | 'printing'
  | 'done'
  | 'failed'
  | 'canceled';

export interface PrintJob {
  id: number;
  contest_id: number | null;
  user_id: number;
  username: string;
  display_name: string | null;
  problem_label: string | null;
  submission_id: number | null;
  language: string;
  filename: string;
  pages_est: number | null;
  pages: number | null;
  location: string | null;
  target_printer: string | null;
  status: PrintStatus;
  claimed_by: string | null;
  claimed_printer: string | null;
  error: string | null;
  created_at: number | null;
  claimed_at: number | null;
  printed_at: number | null;
}

export interface PrintStation {
  name: string;
  location: string | null;
  printers: string[];
  version: string | null;
  queue_seen: number | null;
  last_seen: number | null;
  online: boolean;
}

export interface SubmitResult {
  ok: boolean;
  jobs: number;
  pages: number;
  status: PrintStatus;
}

export interface ListResponse<T> {
  data: T[];
}

export interface PaginationMeta {
  page: number;
  per_page: number;
  total: number;
  total_pages: number;
}

export interface PagedResponse<T> {
  data: T[];
  pagination: PaginationMeta;
}

export interface ArbitraryJobInput {
  contest_id?: number;
  problem_id?: number;
  filename: string;
  language?: string;
  source: string;
}

export interface AdminJobsQuery {
  page: number;
  per_page: number;
  search?: string;
  sort_by?: string;
  sort_order?: 'asc' | 'desc';
  status?: string;
  station?: string;
  printer?: string;
}
