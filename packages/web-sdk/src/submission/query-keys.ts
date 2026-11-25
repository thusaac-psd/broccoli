/**
 * The root of every query that reads a submission as its viewer may see it:
 * a submission's detail, its judgements, its test case rows and the lists
 * that show it. What a viewer may see of a submission changes (a token
 * unlocks it, a contest ends), so one invalidation of `all` reads every such
 * view again and none can be missed.
 */
const SUBMISSION_VIEWS = 'submission-views';

export const submissionViewKeys = {
  all: [SUBMISSION_VIEWS] as const,
  detail: (submissionId: number) =>
    [SUBMISSION_VIEWS, 'detail', submissionId] as const,
  judgements: (submissionId: number) =>
    [SUBMISSION_VIEWS, 'judgements', submissionId] as const,
  judgement: (submissionId: number, judgementId: number | null | undefined) =>
    [SUBMISSION_VIEWS, 'judgement', submissionId, judgementId] as const,
  testCases: (submissionId: number, ...page: unknown[]) =>
    [SUBMISSION_VIEWS, 'test-cases', submissionId, ...page] as const,
  /** A list of submissions, `name` telling the lists apart. */
  list: (name: string, ...filters: unknown[]) =>
    [SUBMISSION_VIEWS, 'list', name, ...filters] as const,
  /**
   * A plugin's own query whose answer depends on what the viewer may see of
   * its submissions (a token count, a breakdown, a board that scores tokened
   * submissions), under the plugin's id. It is read again with every other
   * view, in every tab, so a plugin never follows a visibility change itself.
   */
  plugin: (pluginId: string, ...parts: unknown[]) =>
    [SUBMISSION_VIEWS, 'plugin', pluginId, ...parts] as const,
};
