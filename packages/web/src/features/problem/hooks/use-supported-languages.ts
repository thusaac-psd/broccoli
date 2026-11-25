import { useApiClient } from '@broccoli/web-sdk/api';
import {
  PLUGIN_OUTAGE_POLL_MS,
  pollInterval,
  type Registries,
  registriesQuery,
} from '@broccoli/web-sdk/hooks';
import { useQuery } from '@tanstack/react-query';
import { useCallback } from 'react';

import {
  languagesClosed,
  problemLanguages,
  type ProblemSubmitInputs,
} from '../utils/submit-closure.ts';
import { supportedLanguages } from '../utils/supported-languages.ts';

/**
 * The languages the server can take work in now, read from the one
 * registries query (`registriesQuery`), so every reader on a page shares one
 * request and one invalidation.
 */
export function useSupportedLanguages() {
  return useQuery({
    ...registriesQuery(useApiClient()),
    select: supportedLanguages,
  });
}

/**
 * The languages `problem` takes work in (`problemLanguages`), from the same
 * registries query. While one is closed or none is open, the registries are
 * asked for again at the outage pace, so a language plugin enabled again
 * opens the editor without a reload; the problem's own answer is polled by
 * its view (`problemClosed`).
 */
export function useProblemLanguages(problem: ProblemSubmitInputs | undefined) {
  const select = useCallback(
    (registries: Registries) =>
      problemLanguages(supportedLanguages(registries), problem),
    [problem],
  );
  return useQuery({
    ...registriesQuery(useApiClient()),
    select,
    refetchInterval: pollInterval<Registries>((registries) =>
      registries && languagesClosed(select(registries))
        ? PLUGIN_OUTAGE_POLL_MS
        : false,
    ),
  });
}
