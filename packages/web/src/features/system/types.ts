export interface WorkerInfo {
  id: string;
  started_at: string;
  last_seen: string;
  seconds_since_last_seen: number;
  in_flight: number;
  max_concurrency: number | null;
  sandbox_backend: string;
  version: string;
  hostname?: string | null;
  ip_addresses?: string[];
  os?: string | null;
  arch?: string | null;
  cpu_count?: number | null;
  pid?: number | null;
}

export interface QueueInfo {
  name: string;
  depth: number;
  breakdown: Record<string, number>;
}

export interface SystemOverviewResponse {
  workers: WorkerInfo[];
  queues: QueueInfo[];
  submissions_in_progress: number;
  dlq_unresolved_count: number;
}
