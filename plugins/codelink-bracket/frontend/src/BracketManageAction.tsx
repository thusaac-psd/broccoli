import { useAuth } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { CONTEST_MANAGE } from '@broccoli/web-sdk/permissions';
import { Button, DropdownMenuItem } from '@broccoli/web-sdk/ui';
import { CalendarDays } from 'lucide-react';
import { Link } from 'react-router';

import { managementPath } from './lib/links';

export function BracketManageAction({
  contestId,
  contestType,
  variant = 'panel',
}: {
  contestId?: number;
  contestType?: string;
  variant?: 'panel' | 'menu';
}) {
  const { user } = useAuth();
  const { t } = useTranslation();
  if (
    !contestId ||
    contestType !== 'codelink-bracket' ||
    !user?.permissions.includes(CONTEST_MANAGE)
  )
    return null;
  const link = (
    <Link to={managementPath(contestId)}>
      <CalendarDays className="h-3.5 w-3.5" />
      {t('codelink-bracket.manage.action')}
    </Link>
  );
  return variant === 'menu' ? (
    <DropdownMenuItem asChild>{link}</DropdownMenuItem>
  ) : (
    <Button
      asChild
      variant="ghost"
      size="sm"
      className="w-full justify-start gap-2 h-8 text-xs"
    >
      {link}
    </Button>
  );
}
