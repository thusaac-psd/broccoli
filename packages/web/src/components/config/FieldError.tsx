import type { FieldProblem } from './types';
import { useFieldProblemText } from './use-field-problem-text';

export function FieldError({ problem }: Readonly<{ problem?: FieldProblem }>) {
  const text = useFieldProblemText();
  if (!problem) return null;
  return <p className="text-xs text-destructive mt-1">{text(problem)}</p>;
}
