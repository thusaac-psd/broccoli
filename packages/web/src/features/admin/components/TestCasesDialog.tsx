import { useTranslation } from '@broccoli/web-sdk/i18n';
import type { ProblemSummary } from '@broccoli/web-sdk/problem';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@broccoli/web-sdk/ui';

import { TestCaseList } from '@/features/admin/components/TestCaseList';

export function TestCasesDialog({
  problem,
  open,
  onOpenChange,
}: {
  problem: ProblemSummary;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const { t } = useTranslation();
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-3xl max-h-[90vh] flex flex-col">
        <DialogHeader>
          <DialogTitle>{t('admin.testCases.title')}</DialogTitle>
          <DialogDescription>{problem.title}</DialogDescription>
        </DialogHeader>
        <div className="min-h-0 flex-1 overflow-y-auto">
          <TestCaseList problemId={problem.id} />
        </div>
      </DialogContent>
    </Dialog>
  );
}
