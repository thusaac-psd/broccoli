import { useTranslation } from '@broccoli/web-sdk/i18n';

import type { FieldProblem } from './types';

export function FieldError({ problem }: Readonly<{ problem?: FieldProblem }>) {
  const { t } = useTranslation();
  if (!problem) return null;
  return (
    <p className="text-xs text-destructive mt-1">
      {t(problem.key, problem.params)}
    </p>
  );
}
