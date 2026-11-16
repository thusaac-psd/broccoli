import { currentPath, signInPath, useAuth } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Button, QueryLoading } from '@broccoli/web-sdk/ui';
import { Code2, LogIn } from 'lucide-react';
import { Link, useParams } from 'react-router';

import { PageLayout } from '@/components/PageLayout';
import { useContestInfo } from '@/features/contest/hooks/use-contest-info';
import { ContestSubmissions } from '@/features/submission/components/ContestSubmissions';

export default function ContestSubmissionsPage() {
  const { t } = useTranslation();
  const { contestId } = useParams();
  const { user, isLoading: sessionLoading } = useAuth();
  const id = Number(contestId);
  const { contest } = useContestInfo(id);

  if (!contestId || Number.isNaN(id)) {
    return (
      <div className="flex flex-col gap-4 p-6">
        <h1 className="text-2xl font-bold">{t('contests.notFound')}</h1>
      </div>
    );
  }

  if (sessionLoading) {
    return <QueryLoading className="p-6" />;
  }

  if (!user) {
    return (
      <PageLayout
        pageId="contest-submissions"
        title={t('sidebar.submissions')}
        subtitle={contest?.title}
        icon={<Code2 className="h-6 w-6 text-primary" />}
      >
        <div className="flex flex-col items-center justify-center py-16 text-center">
          <LogIn className="h-10 w-10 text-muted-foreground/40 mb-4" />
          <p className="text-muted-foreground mb-4">
            {t('auth.loginToViewSubmissions')}
          </p>
          <Button asChild>
            <Link to={signInPath(currentPath())}>{t('nav.signIn')}</Link>
          </Button>
        </div>
      </PageLayout>
    );
  }

  return <ContestSubmissions contestId={id} />;
}
