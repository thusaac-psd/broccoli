import type { ApiClient } from '@broccoli/web-sdk/api';
import type {
  Clarification,
  ClarificationReply,
  CreateClarificationBody,
  ReplyClarificationBody,
  ResolveClarificationBody,
} from '@broccoli/web-sdk/clarification';

export async function fetchClarifications(
  apiClient: ApiClient,
  contestId: number,
): Promise<Clarification[]> {
  const { data, error } = await apiClient.GET('/contests/{id}/clarifications', {
    params: { path: { id: contestId } },
  });
  if (error) throw error;
  return (data.data ?? []) as Clarification[];
}

export async function createClarification(
  apiClient: ApiClient,
  contestId: number,
  body: CreateClarificationBody,
  idempotencyKey: string,
): Promise<Clarification> {
  const { data, error } = await apiClient.POST(
    '/contests/{id}/clarifications',
    {
      headers: { 'Idempotency-Key': idempotencyKey },
      params: { path: { id: contestId } },
      body,
    },
  );
  if (error) throw error;
  return data as Clarification;
}

export async function replyClarification(
  apiClient: ApiClient,
  contestId: number,
  clarificationId: number,
  body: ReplyClarificationBody,
  idempotencyKey: string,
): Promise<Clarification> {
  const { data, error } = await apiClient.POST(
    '/contests/{id}/clarifications/{clarification_id}/reply',
    {
      headers: { 'Idempotency-Key': idempotencyKey },
      params: { path: { id: contestId, clarification_id: clarificationId } },
      body,
    },
  );
  if (error) throw error;
  return data as Clarification;
}

export async function resolveClarification(
  apiClient: ApiClient,
  contestId: number,
  clarificationId: number,
  body: ResolveClarificationBody,
): Promise<Clarification> {
  const { data, error } = await apiClient.POST(
    '/contests/{id}/clarifications/{clarification_id}/resolve',
    {
      params: { path: { id: contestId, clarification_id: clarificationId } },
      body,
    },
  );
  if (error) throw error;
  return data as Clarification;
}

export async function toggleReplyPublic(
  apiFetch: (input: string | URL, init?: RequestInit) => Promise<Response>,
  contestId: number,
  clarificationId: number,
  replyId: number,
  includeQuestion?: boolean,
): Promise<ClarificationReply> {
  const params = includeQuestion ? '?include_question=true' : '';
  const res = await apiFetch(
    `/api/v1/contests/${contestId}/clarifications/${clarificationId}/replies/${replyId}/toggle-public${params}`,
    { method: 'POST' },
  );
  if (!res.ok) {
    const body = await res.json().catch(() => ({}));
    throw new Error(body.message ?? 'Failed to toggle reply visibility');
  }
  return res.json();
}
