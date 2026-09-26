import type {
  Clarification,
  ClarificationReply,
} from '@broccoli/web-sdk/clarification';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import {
  Badge,
  Button,
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
  Textarea,
} from '@broccoli/web-sdk/ui';
import { formatTime } from '@broccoli/web-sdk/utils';
import {
  Check,
  CheckCircle2,
  Clock,
  Globe,
  Lock,
  Mail,
  Megaphone,
  MoreHorizontal,
  RotateCcw,
  Send,
  User,
} from 'lucide-react';
import { useState } from 'react';

interface ClarificationCardProps {
  clarification: Clarification;
  isAdmin: boolean;
  currentUserId: number;
  onReply: (content: string) => void;
  onResolve: (resolved: boolean) => void;
  onToggleReplyPublic: (replyId: number, includeQuestion: boolean) => void;
}

export function ClarificationCard({
  clarification,
  isAdmin,
  currentUserId,
  onReply,
  onResolve,
  onToggleReplyPublic,
}: ClarificationCardProps) {
  const [replyContent, setReplyContent] = useState('');
  const { t, locale } = useTranslation();

  const isAnnouncement = clarification.clarification_type === 'announcement';
  const isDirectMessage = clarification.clarification_type === 'direct_message';
  const replies = clarification.replies ?? [];
  const hasReplies = replies.length > 0;
  const isOwn = clarification.author_id === currentUserId;
  const isRecipient = clarification.recipient_id === currentUserId;
  const canReply = isAdmin || isOwn || isRecipient;

  const MAX_REPLY_LENGTH = 10000;
  const replyTrimmed = replyContent.trim();
  const replyValid =
    replyTrimmed.length > 0 && replyTrimmed.length <= MAX_REPLY_LENGTH;

  const handleSubmit = () => {
    if (!replyValid) return;
    onReply(replyContent);
    setReplyContent('');
  };

  return (
    <div className="rounded-lg border bg-card overflow-hidden">
      {/* Header */}
      <div className="flex items-center justify-between px-5 py-3 bg-muted/30 border-b">
        <div className="flex items-center gap-2 text-sm">
          {isAnnouncement ? (
            <Megaphone className="h-4 w-4 text-blue-600" />
          ) : isDirectMessage ? (
            <Mail className="h-4 w-4 text-purple-600" />
          ) : (
            <User className="h-4 w-4 text-muted-foreground" />
          )}
          <span className="font-medium">
            {clarification.author_name}
            {isOwn && (
              <span className="text-xs text-muted-foreground ml-1">
                ({t('clarification.you')})
              </span>
            )}
          </span>
          {isDirectMessage && clarification.recipient_name && (
            <span className="text-xs text-muted-foreground">
              → {clarification.recipient_name}
            </span>
          )}
          <span className="text-muted-foreground text-xs flex items-center gap-1">
            <Clock className="h-3 w-3" />
            {formatTime(clarification.created_at, locale)}
          </span>
        </div>
        <StatusBadge
          clarification={clarification}
          isAnnouncement={isAnnouncement}
          isDirectMessage={isDirectMessage}
        />
      </div>

      {/* Question body */}
      <div className="px-5 py-4">
        <div className="text-sm whitespace-pre-wrap">
          {clarification.content}
        </div>
      </div>

      {/* Replies thread */}
      {replies.length > 0 && (
        <div className="border-t">
          {replies.map((reply, idx) => (
            <ReplyMessage
              key={reply.id}
              reply={reply}
              locale={locale}
              isLast={idx === replies.length - 1}
              isAdmin={isAdmin}
              questionIsPublic={clarification.is_public}
              onTogglePublic={(includeQuestion) =>
                onToggleReplyPublic(reply.id, includeQuestion)
              }
            />
          ))}
        </div>
      )}

      {/* Resolve bar */}
      {!isAnnouncement && canReply && (
        <div className="border-t px-5 py-2 flex items-center justify-between bg-muted/10">
          {clarification.resolved ? (
            <>
              <span className="text-xs text-muted-foreground flex items-center gap-1">
                <CheckCircle2 className="h-3 w-3 text-green-600" />
                {t('clarification.resolvedBy', {
                  name: clarification.resolved_by_name ?? '',
                })}
              </span>
              <Button
                variant="ghost"
                size="sm"
                className="h-7 text-xs gap-1"
                onClick={() => onResolve(false)}
              >
                <RotateCcw className="h-3 w-3" />
                {t('clarification.reopen')}
              </Button>
            </>
          ) : (
            <>
              <span className="text-xs text-muted-foreground">
                {hasReplies
                  ? t('clarification.threadOpen')
                  : t('clarification.awaitingReply')}
              </span>
              {hasReplies && (
                <Button
                  variant="outline"
                  size="sm"
                  className="h-7 text-xs gap-1"
                  onClick={() => onResolve(true)}
                >
                  <Check className="h-3 w-3" />
                  {t('clarification.markResolved')}
                </Button>
              )}
            </>
          )}
        </div>
      )}

      {/* Reply input */}
      {!isAnnouncement && canReply && !clarification.resolved && (
        <div className="border-t px-5 py-4 space-y-3">
          <Textarea
            placeholder={t('clarification.replyPlaceholder')}
            value={replyContent}
            onChange={(e) => setReplyContent(e.target.value)}
            maxLength={MAX_REPLY_LENGTH}
            className="min-h-[80px] text-sm"
          />
          <div className="flex items-center justify-between">
            <span
              className={`text-xs ${replyTrimmed.length > MAX_REPLY_LENGTH ? 'text-destructive' : 'text-muted-foreground'}`}
            >
              {replyTrimmed.length}/{MAX_REPLY_LENGTH}
            </span>
            <Button
              onClick={handleSubmit}
              disabled={!replyValid}
              size="sm"
              className="gap-1"
            >
              <Send className="h-3.5 w-3.5" />
              {t('clarification.send')}
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}

function StatusBadge({
  clarification,
  isAnnouncement,
  isDirectMessage,
}: {
  clarification: Clarification;
  isAnnouncement: boolean;
  isDirectMessage: boolean;
}) {
  const { t } = useTranslation();

  if (isAnnouncement) {
    return (
      <Badge
        variant="outline"
        className="gap-1 border-blue-200 bg-blue-50 text-blue-700 dark:bg-blue-950 dark:border-blue-800 dark:text-blue-300"
      >
        <Megaphone className="h-3 w-3" /> {t('clarification.announcement')}
      </Badge>
    );
  }
  if (isDirectMessage) {
    return (
      <Badge
        variant="outline"
        className="gap-1 border-purple-200 bg-purple-50 text-purple-700 dark:bg-purple-950 dark:border-purple-800 dark:text-purple-300"
      >
        <Mail className="h-3 w-3" /> {t('clarification.direct')}
      </Badge>
    );
  }
  if (clarification.resolved) {
    return (
      <Badge className="gap-1 bg-green-600 hover:bg-green-700 text-white">
        <CheckCircle2 className="h-3 w-3" /> {t('clarification.statusResolved')}
      </Badge>
    );
  }
  if (clarification.replies?.length > 0) {
    return (
      <Badge
        variant="outline"
        className="gap-1 border-blue-200 bg-blue-50 text-blue-700 dark:bg-blue-950 dark:border-blue-800 dark:text-blue-300"
      >
        {t('clarification.statusInProgress')}
      </Badge>
    );
  }
  return (
    <Badge
      variant="secondary"
      className="text-yellow-600 bg-yellow-50 hover:bg-yellow-100 dark:bg-yellow-950 dark:text-yellow-400"
    >
      {t('clarification.statusPending')}
    </Badge>
  );
}

function ReplyMessage({
  reply,
  locale,
  isLast,
  isAdmin,
  questionIsPublic,
  onTogglePublic,
}: {
  reply: ClarificationReply;
  locale: string;
  isLast: boolean;
  isAdmin: boolean;
  questionIsPublic: boolean;
  onTogglePublic: (includeQuestion: boolean) => void;
}) {
  const { t } = useTranslation();

  return (
    <div
      className={`px-5 py-3 flex gap-3 ${!isLast ? 'border-b' : ''} hover:bg-muted/20 transition-colors`}
    >
      <div className="shrink-0 mt-0.5">
        <div className="h-6 w-6 rounded-full bg-muted flex items-center justify-center">
          <User className="h-3.5 w-3.5 text-muted-foreground" />
        </div>
      </div>
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2 mb-1">
          <span className="text-sm font-medium">{reply.author_name}</span>
          <span className="text-xs text-muted-foreground">
            {formatTime(reply.created_at, locale)}
          </span>
          {reply.is_public ? (
            <Badge
              variant="outline"
              className="h-5 text-[10px] gap-0.5 border-blue-200 bg-blue-50 text-blue-700 dark:bg-blue-950 dark:border-blue-800 dark:text-blue-300"
            >
              <Megaphone className="h-2.5 w-2.5" /> {t('clarification.public')}
            </Badge>
          ) : (
            <Badge
              variant="outline"
              className="h-5 text-[10px] gap-0.5 border-amber-200 bg-amber-50 text-amber-700 dark:bg-amber-950 dark:border-amber-800 dark:text-amber-300"
            >
              <Lock className="h-2.5 w-2.5" /> {t('clarification.private')}
            </Badge>
          )}
          {isAdmin && (
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <button
                  type="button"
                  className="ml-auto text-muted-foreground hover:text-foreground transition-colors rounded p-0.5 hover:bg-muted"
                >
                  <MoreHorizontal className="h-4 w-4" />
                </button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                {reply.is_public ? (
                  <DropdownMenuItem onClick={() => onTogglePublic(false)}>
                    <Lock className="h-4 w-4 mr-2" />
                    {t('clarification.makePrivate')}
                  </DropdownMenuItem>
                ) : (
                  <>
                    <DropdownMenuItem onClick={() => onTogglePublic(false)}>
                      <Globe className="h-4 w-4 mr-2" />
                      {t('clarification.publishReplyOnly')}
                    </DropdownMenuItem>
                    {!questionIsPublic && (
                      <DropdownMenuItem onClick={() => onTogglePublic(true)}>
                        <Megaphone className="h-4 w-4 mr-2" />
                        {t('clarification.publishWithQuestion')}
                      </DropdownMenuItem>
                    )}
                  </>
                )}
              </DropdownMenuContent>
            </DropdownMenu>
          )}
        </div>
        <div className="text-sm text-foreground/90 whitespace-pre-wrap">
          {reply.content}
        </div>
      </div>
    </div>
  );
}
