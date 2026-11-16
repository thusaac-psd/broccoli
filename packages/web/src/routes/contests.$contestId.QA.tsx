import { currentPath, signInPath, useAuth } from '@broccoli/web-sdk/auth';
import { queryState } from '@broccoli/web-sdk/hooks';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { CONTEST_MANAGE } from '@broccoli/web-sdk/permissions';
import {
  Button,
  QueryErrorState,
  QueryLoading,
  Skeleton,
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
} from '@broccoli/web-sdk/ui';
import { LogIn, MessageCircle } from 'lucide-react';
import { useEffect } from 'react';
import { Link, useParams, useSearchParams } from 'react-router';

import { PageLayout } from '@/components/PageLayout';
import { AskQuestionDialog } from '@/features/clarification/components/AskQuestionDialog';
import { ClarificationCard } from '@/features/clarification/components/ClarificationCard';
import { PostAnnouncementDialog } from '@/features/clarification/components/PostAnnouncementDialog';
import { SendDirectMessageDialog } from '@/features/clarification/components/SendDirectMessageDialog';
import {
  useClarifications,
  useCreateClarification,
  useReplyClarification,
  useResolveClarification,
  useToggleReplyPublic,
} from '@/features/clarification/hooks/use-clarifications';

type QaTab = 'all' | 'announcements' | 'questions' | 'dm';

function parseQaTab(value: string | null): QaTab {
  return value === 'announcements' || value === 'questions' || value === 'dm'
    ? value
    : 'all';
}

export default function ContestQAPage() {
  const { t } = useTranslation();
  const { user, isLoading: authLoading } = useAuth();
  const { contestId } = useParams();
  const [searchParams, setSearchParams] = useSearchParams();
  const id = Number(contestId ?? '0');

  const isAdmin = !!user?.permissions?.includes(CONTEST_MANAGE);

  const clarificationsQuery = useClarifications(
    id,
    !!user && !!contestId && !Number.isNaN(id),
  );
  const listState = queryState(clarificationsQuery);
  const loaded = listState.status === 'data' || listState.status === 'empty';
  const isLoading = !loaded;
  const clarifications = loaded ? listState.data : [];
  const createMutation = useCreateClarification(id);
  const replyMutation = useReplyClarification(id);
  const resolveMutation = useResolveClarification(id);
  const togglePublicMutation = useToggleReplyPublic(id);

  const announcements = clarifications.filter(
    (c) => c.clarification_type === 'announcement',
  );
  const questions = clarifications.filter(
    (c) => c.clarification_type === 'question',
  );
  const directMessages = clarifications.filter(
    (c) => c.clarification_type === 'direct_message',
  );

  const showDmTab = isAdmin || directMessages.length > 0;
  const requestedTab = parseQaTab(searchParams.get('tab'));
  const canKeepDmTab = requestedTab === 'dm' && (isLoading || showDmTab);
  const activeTab = canKeepDmTab
    ? 'dm'
    : !showDmTab && requestedTab === 'dm'
      ? 'all'
      : requestedTab;

  useEffect(() => {
    if (!isLoading && !showDmTab && requestedTab === 'dm') {
      const nextParams = new URLSearchParams(searchParams);
      nextParams.delete('tab');
      setSearchParams(nextParams, { replace: true });
    }
  }, [isLoading, requestedTab, searchParams, setSearchParams, showDmTab]);

  const setActiveTab = (tab: string) => {
    const nextTab = parseQaTab(tab);
    const nextParams = new URLSearchParams(searchParams);

    if (nextTab === 'all') {
      nextParams.delete('tab');
    } else {
      nextParams.set('tab', nextTab);
    }

    setSearchParams(nextParams);
  };

  if (!contestId || Number.isNaN(Number(contestId))) {
    return <div className="text-2xl font-bold">{t('contests.notFound')}</div>;
  }

  // Counts appear once the list has loaded; until then, or after a failed
  // load, there is nothing to count.
  const counted = (label: string, items: unknown[]) =>
    loaded ? `${label} (${items.length})` : label;

  const renderList = (items: typeof clarifications, emptyMessage: string) => {
    if (listState.status === 'error') {
      return (
        <QueryErrorState
          error={listState.error}
          retry={listState.retry}
          retrying={listState.retrying}
          failure={t('contest.qa.loadFailed')}
        />
      );
    }
    if (!loaded) {
      return (
        <div role="status" aria-busy className="space-y-4">
          <span className="sr-only">{t('common.loading')}</span>
          <Skeleton className="h-32 w-full" />
          <Skeleton className="h-32 w-full" />
        </div>
      );
    }

    if (items.length === 0) {
      return (
        <div className="rounded-lg border border-dashed p-8 text-center text-sm text-muted-foreground">
          <MessageCircle className="mx-auto h-10 w-10 mb-2 opacity-20" />
          <p>{emptyMessage}</p>
        </div>
      );
    }

    return (
      <div className="space-y-4">
        {items.map((c) => (
          <ClarificationCard
            key={c.id}
            clarification={c}
            isAdmin={isAdmin}
            currentUserId={user?.id ?? -1}
            onReply={(content) =>
              replyMutation.mutateAsync({
                clarificationId: c.id,
                content,
              })
            }
            onResolve={(resolved) =>
              resolveMutation.mutate({
                clarificationId: c.id,
                resolved,
              })
            }
            onToggleReplyPublic={(replyId) =>
              togglePublicMutation.mutate({
                clarificationId: c.id,
                replyId,
              })
            }
          />
        ))}
      </div>
    );
  };

  if (authLoading) {
    return (
      <PageLayout
        pageId="contest-qa"
        title={t('sidebar.qa')}
        icon={<MessageCircle className="h-6 w-6 text-primary" />}
      >
        <QueryLoading />
      </PageLayout>
    );
  }

  if (!user) {
    return (
      <PageLayout
        pageId="contest-qa"
        title={t('sidebar.qa')}
        icon={<MessageCircle className="h-6 w-6 text-primary" />}
      >
        <div className="flex flex-col items-center justify-center py-16 text-center">
          <LogIn className="h-10 w-10 text-muted-foreground/40 mb-4" />
          <p className="text-muted-foreground mb-4">{t('auth.loginToAsk')}</p>
          <Button asChild>
            <Link to={signInPath(currentPath())}>{t('nav.signIn')}</Link>
          </Button>
        </div>
      </PageLayout>
    );
  }

  return (
    <PageLayout
      pageId="contest-qa"
      title={t('sidebar.qa')}
      icon={<MessageCircle className="h-6 w-6 text-primary" />}
      actions={
        <div className="flex items-center gap-2">
          {isAdmin && (
            <>
              <PostAnnouncementDialog
                onSubmit={(content) =>
                  createMutation.mutateAsync({
                    content,
                    clarification_type: 'announcement',
                  })
                }
              />
              <SendDirectMessageDialog
                contestId={id}
                onSubmit={(content, recipientId) =>
                  createMutation.mutateAsync({
                    content,
                    clarification_type: 'direct_message',
                    recipient_id: recipientId,
                  })
                }
              />
            </>
          )}
          <AskQuestionDialog
            onSubmit={(content) =>
              createMutation.mutateAsync({
                content,
                clarification_type: 'question',
              })
            }
          />
        </div>
      }
      contentClassName="flex flex-col gap-6"
    >
      <Tabs value={activeTab} onValueChange={setActiveTab}>
        <TabsList>
          <TabsTrigger value="all">
            {counted(t('contest.qa.all'), clarifications)}
          </TabsTrigger>
          <TabsTrigger value="announcements">
            {counted(t('contest.qa.announcements'), announcements)}
          </TabsTrigger>
          <TabsTrigger value="questions">
            {counted(t('contest.qa.questions'), questions)}
          </TabsTrigger>
          {(showDmTab || canKeepDmTab) && (
            <TabsTrigger value="dm">
              {counted(
                isAdmin
                  ? t('contest.qa.directMessages')
                  : t('contest.qa.messages'),
                directMessages,
              )}
            </TabsTrigger>
          )}
        </TabsList>

        <TabsContent value="all">
          {renderList(clarifications, t('contest.qa.empty'))}
        </TabsContent>
        <TabsContent value="announcements">
          {renderList(announcements, t('contest.qa.empty'))}
        </TabsContent>
        <TabsContent value="questions">
          {renderList(questions, t('contest.qa.empty'))}
        </TabsContent>
        {(showDmTab || canKeepDmTab) && (
          <TabsContent value="dm">
            {renderList(directMessages, t('contest.qa.empty'))}
          </TabsContent>
        )}
      </Tabs>
    </PageLayout>
  );
}
