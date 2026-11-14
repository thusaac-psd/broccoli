import { useTranslation } from '@broccoli/web-sdk/i18n';
import { useCallback } from 'react';

import type { FieldProblem } from './types';

/** How a field problem reads in the UI language. */
export function useFieldProblemText(): (problem: FieldProblem) => string {
  const { t, lookup } = useTranslation();
  return useCallback(
    (problem: FieldProblem) => {
      if ('key' in problem) return t(problem.key, problem.params);
      const translated =
        problem.pluginKey === null
          ? undefined
          : lookup(problem.pluginKey, problem.params);
      return (
        translated ??
        t('plugins.config.serverFieldError', { message: problem.message })
      );
    },
    [t, lookup],
  );
}
