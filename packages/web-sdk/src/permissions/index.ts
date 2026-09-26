// Code generated from packages/broccoli-types/src/permissions.rs. DO NOT EDIT.
// Regenerate: `REGEN_PERMISSIONS_TS=1 cargo test -p broccoli-types permissions`.

export const SUBMISSION_SUBMIT = 'submission:submit';
export const SUBMISSION_VIEW_ALL = 'submission:view_all';
export const SUBMISSION_REJUDGE = 'submission:rejudge';
export const PROBLEM_CREATE = 'problem:create';
export const PROBLEM_EDIT = 'problem:edit';
export const PROBLEM_DELETE = 'problem:delete';
export const CONTEST_CREATE = 'contest:create';
export const CONTEST_MANAGE = 'contest:manage';
export const CONTEST_DELETE = 'contest:delete';
export const USER_MANAGE = 'user:manage';
export const ROLE_MANAGE = 'role:manage';
export const PLUGIN_MANAGE = 'plugin:manage';
export const TIMER = 'timer';
export const DLQ_MANAGE = 'dlq:manage';
export const SYSTEM_VIEW = 'system:view';
export const SYSTEM_ADMIN = 'system:admin';

export const ALL_PERMISSIONS = [
  SUBMISSION_SUBMIT,
  SUBMISSION_VIEW_ALL,
  SUBMISSION_REJUDGE,
  PROBLEM_CREATE,
  PROBLEM_EDIT,
  PROBLEM_DELETE,
  CONTEST_CREATE,
  CONTEST_MANAGE,
  CONTEST_DELETE,
  USER_MANAGE,
  ROLE_MANAGE,
  PLUGIN_MANAGE,
  TIMER,
  DLQ_MANAGE,
  SYSTEM_VIEW,
  SYSTEM_ADMIN,
] as const;

export type Permission = (typeof ALL_PERMISSIONS)[number];
