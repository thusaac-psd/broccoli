// Host page paths the bracket links into. Kept in one place so a host route
// change is a one-line fix here.

export const problemPath = (contestId: number, problemId: number) =>
  `/contests/${contestId}/problems/${problemId}`;

export const submissionPath = (contestId: number, submissionId: number) =>
  `/contests/${contestId}/submissions/${submissionId}`;
