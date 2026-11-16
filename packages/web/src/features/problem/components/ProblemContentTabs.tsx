import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Button } from '@broccoli/web-sdk/ui';
import { ArrowLeft, Code2, Edit } from 'lucide-react';
import type { ReactNode } from 'react';

export type ProblemViewTab = 'description' | 'coding';

interface ProblemContentTabsProps {
  activeTab: ProblemViewTab;
  onTabChange: (tab: ProblemViewTab) => void;
  descriptionContent: ReactNode;
  /** `null` when nothing may be submitted here: no coding tab is offered. */
  codingContent: ReactNode | null;
  canEdit: boolean;
  onEdit: () => void;
}

export function ProblemContentTabs({
  activeTab,
  onTabChange,
  descriptionContent,
  codingContent,
  canEdit,
  onEdit,
}: ProblemContentTabsProps) {
  const { t } = useTranslation();
  const isDescription = activeTab === 'description' || codingContent === null;

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex shrink-0 items-center justify-between border-b bg-background px-6 py-1.5">
        {isDescription ? (
          <>
            <span className="text-sm font-semibold text-foreground">
              {t('problem.description')}
            </span>
            <div className="flex items-center gap-2">
              {canEdit && (
                <Button
                  onClick={onEdit}
                  size="sm"
                  variant="default"
                  className="h-8 gap-1.5 bg-primary px-4 font-semibold text-primary-foreground shadow-sm hover:bg-primary/90"
                >
                  <Edit className="h-3.5 w-3.5" />
                  {t('problem.edit')}
                </Button>
              )}
              {codingContent !== null && (
                <Button
                  onClick={() => onTabChange('coding')}
                  size="sm"
                  variant="default"
                  className="h-8 gap-1.5 bg-primary px-4 font-semibold text-primary-foreground shadow-sm hover:bg-primary/90"
                >
                  <Code2 className="h-3.5 w-3.5" />
                  {t('editor.submit')}
                </Button>
              )}
            </div>
          </>
        ) : (
          <>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => onTabChange('description')}
              className="-ml-2 h-8 gap-1.5 text-sm font-semibold text-foreground hover:text-foreground"
            >
              <ArrowLeft className="h-3.5 w-3.5" />
              {t('problem.backToDescription')}
            </Button>
            <div className="h-8" />
          </>
        )}
      </div>

      {isDescription ? descriptionContent : codingContent}
    </div>
  );
}
