import type { components } from '@/api/schema';

/**
 * `clarification_type` is validated server-side against a closed set
 * (`VALID_TYPES` in `packages/server/src/models/clarification.rs`), but the
 * backing column - and therefore this field in the generated schema - is a
 * plain SQL `String`, not a typed enum, so it comes through the schema as
 * bare `string`. Hand-declared here for the same reason `problem/types.ts`
 * hand-declares `TestCaseMergeStrategy`: the schema doesn't carry the closed
 * set, but the client still wants it caught at compile time.
 */
export type ClarificationType = 'announcement' | 'question' | 'direct_message';

export type ClarificationReply =
  components['schemas']['ClarificationReplyResponse'];

export type Clarification = Omit<
  components['schemas']['ClarificationResponse'],
  'clarification_type'
> & { clarification_type: ClarificationType };

export type ClarificationListResponse =
  components['schemas']['ClarificationListResponse'];

export type CreateClarificationBody = Omit<
  components['schemas']['CreateClarificationRequest'],
  'clarification_type'
> & { clarification_type: ClarificationType };

export type ReplyClarificationBody =
  components['schemas']['ReplyClarificationRequest'];

export type ResolveClarificationBody =
  components['schemas']['ResolveClarificationRequest'];
