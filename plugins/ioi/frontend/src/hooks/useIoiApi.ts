import { usePluginApi } from '@broccoli/web-sdk/api';
import { useMemo } from 'react';

import type {
  ContestInfoResponse,
  ScoreboardResponse,
  SubmissionStatusResponse,
  SubtaskScoresResponse,
  TaskConfigResponse,
  TokenStatusResponse,
  UseTokenResponse,
} from '../types';

const ROUTES = '/api/plugins/ioi';

export function useIoiApi() {
  const api = usePluginApi('ioi');

  return useMemo(
    () => ({
      getContestInfo: (contestId: number) =>
        api.get<ContestInfoResponse>(`${ROUTES}/contests/${contestId}/info`),

      getScoreboard: (contestId: number) =>
        api.get<ScoreboardResponse>(
          `${ROUTES}/contests/${contestId}/scoreboard`,
        ),

      getTaskConfig: (contestId: number, problemId: number) =>
        api.get<TaskConfigResponse>(
          `${ROUTES}/contests/${contestId}/problems/${problemId}/config`,
        ),

      getTokenStatus: (contestId: number) =>
        api.get<TokenStatusResponse>(
          `${ROUTES}/contests/${contestId}/token-status`,
        ),

      useToken: (contestId: number, submissionId: number) =>
        api.post<UseTokenResponse>(
          `${ROUTES}/contests/${contestId}/submissions/${submissionId}/token`,
        ),

      getSubmissionSubtaskScores: (contestId: number, submissionId: number) =>
        api.get<SubtaskScoresResponse>(
          `${ROUTES}/contests/${contestId}/submissions/${submissionId}/subtask-scores`,
        ),

      getSubmissionStatus: (contestId: number, problemId: number) =>
        api.get<SubmissionStatusResponse>(
          `${ROUTES}/contests/${contestId}/problems/${problemId}/submission-status`,
        ),
    }),
    [api],
  );
}
