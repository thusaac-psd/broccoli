import type { ContestInfoResponse, TaskConfigResponse } from './types';

type EffectiveFeedback = 'none' | 'total_only' | 'subtask_scores' | 'full';

interface ResolveFeedbackVisibilityInput {
  taskConfig: Pick<TaskConfigResponse, 'feedback_level'>;
  contestInfo?: Pick<ContestInfoResponse, 'token_mode'> | null;
  isTokened: boolean;
  canViewPrivilegedSubmissionFeedback: boolean;
}

export interface FeedbackVisibility {
  effectiveFeedback: EffectiveFeedback;
  usesTokenMode: boolean;
  needsTokenStatus: boolean;
}

export function resolveFeedbackVisibility({
  taskConfig,
  contestInfo,
  isTokened,
  canViewPrivilegedSubmissionFeedback,
}: ResolveFeedbackVisibilityInput): FeedbackVisibility {
  const usesTokenMode = contestInfo?.token_mode !== 'none';

  if (canViewPrivilegedSubmissionFeedback) {
    return {
      effectiveFeedback: 'full',
      usesTokenMode,
      needsTokenStatus: false,
    };
  }

  if (usesTokenMode && isTokened) {
    return {
      effectiveFeedback: 'full',
      usesTokenMode,
      needsTokenStatus: true,
    };
  }

  return {
    effectiveFeedback: taskConfig.feedback_level,
    usesTokenMode,
    needsTokenStatus: usesTokenMode,
  };
}

/**
 * Whether the host's own view of a submission's result shows, with nothing
 * from this plugin: it failed to compile (the host shows the compiler's
 * output), or its outcome is withheld from the viewer (the host says why,
 * and there is no result to break down).
 */
export function hostShowsResult(submission: {
  status?: string | null;
  withheld?: unknown;
}): boolean {
  return submission.status === 'CompilationError' || !!submission.withheld;
}
