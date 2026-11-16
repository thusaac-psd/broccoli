import type { components } from '@/api/schema';

export type Contest = components['schemas']['ContestResponse'];
export type ContestSummary = components['schemas']['ContestListItem'];
/** Where a contest is now, on the server's clock (see `ContestResponse.phase`). */
export type ContestPhase = components['schemas']['ContestPhase'];
export type ContestProblem = components['schemas']['ContestProblemResponse'];
