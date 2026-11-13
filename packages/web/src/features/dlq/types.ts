export interface DlqMessage {
  id: number;
  message_id: string;
  message_type: string;
  submission_id: number | null;
  error_code: string;
  error_message: string;
  retry_count: number;
  first_failed_at: string;
  created_at: string;
  last_failed_at: string;
  resolved: boolean;
  resolved_at: string | null;
  /** The operator who resolved it; null when it resolved itself. */
  resolved_by: number | null;
}

export interface DlqMessageDetail extends DlqMessage {
  payload: unknown;
  retry_history: unknown;
}

export interface DlqListResponse {
  data: DlqMessage[];
  pagination: {
    page: number;
    per_page: number;
    total: number;
    total_pages: number;
  };
}

export interface DlqStats {
  total_unresolved: number;
  total_resolved: number;
  unresolved_by_message_type: {
    operation_task: number;
    stuck_submission: number;
    stuck_code_run: number;
    stuck_submission_judgement: number;
  };
  unresolved_by_error_code: Record<string, number>;
}

export type DlqResolvedFilter = 'all' | 'unresolved' | 'resolved';
