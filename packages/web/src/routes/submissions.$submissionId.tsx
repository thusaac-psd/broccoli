import { useTranslation } from '@broccoli/web-sdk/i18n';
import { FileText } from 'lucide-react';
import { Navigate, useParams } from 'react-router';

import { PageLayout } from '@/components/PageLayout';
import { SubmissionDetailView } from '@/features/submission/components/SubmissionDetailView';
import { pageTitle, type RouteHandle } from '@/lib/route-title';

export const handle: RouteHandle = pageTitle('result.title');

export default function StandaloneSubmissionDetail() {
  const { t } = useTranslation();
  const { submissionId } = useParams();
  const sid = Number(submissionId);

  if (isNaN(sid)) {
    return <Navigate to="/" replace />;
  }

  return (
    <PageLayout
      pageId="submission-detail"
      title={`${t('result.title')} #${sid}`}
      icon={<FileText className="h-6 w-6 text-primary" />}
    >
      <SubmissionDetailView submissionId={sid} />
    </PageLayout>
  );
}
