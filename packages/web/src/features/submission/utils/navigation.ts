import type { User } from '@broccoli/web-sdk/auth';
import {
  CONTEST_MANAGE,
  PLUGIN_MANAGE,
  PROBLEM_CREATE,
  SUBMISSION_VIEW_ALL,
  USER_MANAGE,
} from '@broccoli/web-sdk/permissions';

import type { MessageKey } from '@/lib/i18n/catalog';

type SourceKind =
  | 'allSubmissions'
  | 'contestSubmissions'
  | 'coding'
  | 'overview';

export interface SubmissionReturnContext {
  kind: SourceKind;
  href: string;
  locationKey: string;
  scrollKey: string;
  scrollTop: number;
  userId: number;
}

interface NavigationLocation {
  pathname: string;
  search: string;
  hash: string;
  key: string;
  state: unknown;
}

const BACK_LABELS = {
  allSubmissions: 'submissionDetail.backToAllSubmissions',
  contestSubmissions: 'submissionDetail.backToContestSubmissions',
  coding: 'submissionDetail.backToCoding',
  overview: 'submissionDetail.backToOverview',
} as const;

export interface SubmissionReturnTarget {
  to: string;
  labelKey: MessageKey;
  source?: SubmissionReturnContext;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

function isLocationKey(value: unknown): value is string {
  return typeof value === 'string' && value.length > 0 && value.length <= 256;
}

function sourceKind(href: string): SourceKind | null {
  // Only canonical, root-relative application paths can be return targets.
  // In particular, reject protocol-relative URLs and backslash normalization.
  if (
    !href.startsWith('/') ||
    href.startsWith('//') ||
    href.includes('\\') ||
    Array.from(href).some((character) => character.charCodeAt(0) <= 32)
  ) {
    return null;
  }
  const url = new URL(href, 'https://broccoli.invalid');
  if (href !== url.pathname + url.search + url.hash) return null;

  if (url.pathname === '/admin/submissions') return 'allSubmissions';
  if (url.pathname === '/admin') return 'overview';
  if (/^\/contests\/[1-9]\d*\/submissions$/.test(url.pathname)) {
    return 'contestSubmissions';
  }
  if (
    /^(?:\/contests\/[1-9]\d*)?\/problems\/[1-9]\d*$/.test(url.pathname) &&
    url.searchParams.get('tab') === 'coding'
  ) {
    return 'coding';
  }
  return null;
}

function canReturnTo(kind: SourceKind, user: User) {
  if (kind === 'allSubmissions') {
    return user.permissions.includes(SUBMISSION_VIEW_ALL);
  }
  if (kind === 'overview') {
    // The same entry permissions as the Dashboard item in Sidebar.tsx.
    return [USER_MANAGE, PROBLEM_CREATE, CONTEST_MANAGE, PLUGIN_MANAGE].some(
      (permission) => user.permissions.includes(permission),
    );
  }
  return true;
}

export function getSubmissionScrollRestoration(location: NavigationLocation) {
  const restore = isRecord(location.state)
    ? location.state.submissionScroll
    : null;
  if (
    isRecord(restore) &&
    restore.href === location.pathname + location.search + location.hash &&
    sourceKind(restore.href as string) &&
    isLocationKey(restore.key) &&
    typeof restore.top === 'number' &&
    Number.isFinite(restore.top) &&
    restore.top >= 0
  ) {
    return { key: restore.key, top: restore.top };
  }
  return undefined;
}

export function getSubmissionScrollKey(location: NavigationLocation): string {
  return getSubmissionScrollRestoration(location)?.key ?? location.key;
}

export function createSubmissionNavigationState(
  location: NavigationLocation,
  user: User | null,
  scrollTop = 0,
) {
  const href = location.pathname + location.search + location.hash;
  const kind = sourceKind(href);
  if (!kind || !user || !canReturnTo(kind, user)) return undefined;

  return {
    submissionReturn: {
      kind,
      href,
      locationKey: location.key,
      scrollKey: getSubmissionScrollKey(location),
      scrollTop,
      userId: user.id,
    } satisfies SubmissionReturnContext,
  };
}

export function readSubmissionReturnContext(
  state: unknown,
  user: User | null,
): SubmissionReturnContext | undefined {
  const source = isRecord(state) ? state.submissionReturn : null;
  if (
    !user ||
    !isRecord(source) ||
    source.userId !== user.id ||
    typeof source.href !== 'string' ||
    !isLocationKey(source.locationKey) ||
    !isLocationKey(source.scrollKey) ||
    typeof source.scrollTop !== 'number' ||
    !Number.isFinite(source.scrollTop) ||
    source.scrollTop < 0
  ) {
    return undefined;
  }
  const kind = sourceKind(source.href);
  if (!kind || source.kind !== kind || !canReturnTo(kind, user))
    return undefined;

  return {
    kind,
    href: source.href,
    locationKey: source.locationKey,
    scrollKey: source.scrollKey,
    scrollTop: source.scrollTop,
    userId: user.id,
  };
}

export function resolveSubmissionReturn({
  source,
  contestId,
  canViewContest,
  canViewAll,
  problemId,
  canViewProblem,
}: {
  source?: SubmissionReturnContext;
  contestId?: number | null;
  canViewContest: boolean;
  canViewAll: boolean;
  problemId?: number;
  canViewProblem: boolean;
}): SubmissionReturnTarget {
  if (source) {
    return { to: source.href, labelKey: BACK_LABELS[source.kind], source };
  }
  if (canViewContest && contestId != null && contestId > 0) {
    return {
      to: '/contests/' + contestId + '/submissions',
      labelKey: 'submissionDetail.viewContestSubmissions',
    };
  }
  if (canViewAll) {
    return {
      to: '/admin/submissions',
      labelKey: 'submissionDetail.viewAllSubmissions',
    };
  }
  if (canViewProblem && problemId != null && problemId > 0) {
    return {
      to: '/problems/' + problemId,
      labelKey: 'submissionDetail.openProblem',
    };
  }
  return { to: '/', labelKey: 'error.backToHome' };
}

export function submissionReturnState(target: SubmissionReturnTarget) {
  return target.source
    ? {
        submissionScroll: {
          href: target.to,
          key: target.source.scrollKey,
          top: target.source.scrollTop,
        },
      }
    : undefined;
}

export interface NavigationHistory {
  keys: string[];
  index: number;
}

/** Track only history entries observed in this document; a reload starts fresh. */
export function recordNavigation(
  history: NavigationHistory,
  key: string,
  action: 'PUSH' | 'POP' | 'REPLACE',
): NavigationHistory {
  if (history.keys[history.index] === key) return history;
  if (action === 'PUSH') {
    const keys = [...history.keys.slice(0, history.index + 1), key];
    return { keys, index: keys.length - 1 };
  }
  if (action === 'REPLACE') {
    const keys = [...history.keys];
    keys[history.index] = key;
    return { keys, index: history.index };
  }
  const index = history.keys.indexOf(key);
  return index === -1 ? { keys: [key], index: 0 } : { ...history, index };
}
