import type { components } from '@/api/schema';
import {
  CONTEST_CREATE,
  CONTEST_DELETE,
  CONTEST_MANAGE,
  DLQ_MANAGE,
  PLUGIN_MANAGE,
  PROBLEM_CREATE,
  PROBLEM_DELETE,
  PROBLEM_EDIT,
  SUBMISSION_REJUDGE,
  SUBMISSION_VIEW_ALL,
  SYSTEM_VIEW,
  USER_MANAGE,
} from '@/permissions';

export type User = components['schemas']['MeResponse'];
export type LoginRequest = components['schemas']['LoginRequest'];

// Permissions that grant some elevated (admin-area) UI access. Curated subset of
// the catalog -- it intentionally omits self-service permissions like
// `submission:submit` and the `role:manage` / `system:admin` super-grants; the
// values come from `@/permissions` so they cannot drift from the server.
export const USER_PERMISSIONS = [
  SUBMISSION_VIEW_ALL,
  SUBMISSION_REJUDGE,
  PROBLEM_CREATE,
  PROBLEM_EDIT,
  PROBLEM_DELETE,
  CONTEST_CREATE,
  CONTEST_MANAGE,
  CONTEST_DELETE,
  PLUGIN_MANAGE,
  USER_MANAGE,
  DLQ_MANAGE,
  SYSTEM_VIEW,
];

export type CreatedUser = components['schemas']['CreatedUserResponse'];
