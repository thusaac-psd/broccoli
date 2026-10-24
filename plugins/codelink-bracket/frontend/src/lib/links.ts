// Host page paths the bracket links into. Kept in one place so a host route
// change is a one-line fix here.

export const problemPath = (contestId: number, problemId: number) =>
  `/contests/${contestId}/problems/${problemId}`;

export const submissionPath = (contestId: number, submissionId: number) =>
  `/contests/${contestId}/submissions/${submissionId}`;

export const MANAGEMENT_ROUTE = '/contests/:contestId/bracket-management';
export const managementPath = (contestId: number) =>
  MANAGEMENT_ROUTE.replace(':contestId', String(contestId));
