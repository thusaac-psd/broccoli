import { useAuth } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Button } from '@broccoli/web-sdk/ui';
import { Bell, BellRing, X } from 'lucide-react';
import { useEffect, useState } from 'react';
import { Link, useNavigate } from 'react-router';

import { useGameNotifications } from './hooks/useGameNotifications';
import { useMyMatch } from './hooks/useMyMatch';
import {
  claimDesktopNotification,
  type GameNotification,
  gameNotificationText,
  readStorage,
  writeStorage,
} from './lib/notifications';

type Permission = NotificationPermission | 'unsupported';

function notificationPermission(): Permission {
  return typeof window !== 'undefined' &&
    window.isSecureContext &&
    'Notification' in window
    ? Notification.permission
    : 'unsupported';
}

/** Mounted once in the contest layout; results belong to players, not spectators. */
export function BracketNotifications({ contestId }: { contestId?: number }) {
  const { user } = useAuth();
  const mine = useMyMatch(contestId);
  if (!contestId || !user || !mine) return null;
  return (
    <NotificationCenter
      key={`${contestId}:${user.id}`}
      contestId={contestId}
      viewerId={user.id}
    />
  );
}

function NotificationCenter({
  contestId,
  viewerId,
}: {
  contestId: number;
  viewerId: number;
}) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const preferenceKey = `codelink:desktop:${viewerId}`;
  const [permission, setPermission] = useState<Permission>(
    notificationPermission,
  );
  const [enabled, setEnabled] = useState(
    () => readStorage('localStorage', preferenceKey) === 'true',
  );
  const [messages, setMessages] = useState<GameNotification[]>([]);
  const [error, setError] = useState(false);

  useEffect(() => {
    const refresh = () => {
      setPermission(notificationPermission());
      const saved = readStorage('localStorage', preferenceKey);
      if (saved !== null) setEnabled(saved === 'true');
    };
    window.addEventListener('focus', refresh);
    window.addEventListener('storage', refresh);
    return () => {
      window.removeEventListener('focus', refresh);
      window.removeEventListener('storage', refresh);
    };
  }, [preferenceKey]);

  const showDesktop = (title: string, body: string, tag: string) => {
    try {
      const notification = new Notification(title, { body, tag });
      notification.onclick = () => {
        window.focus();
        navigate(`/contests/${contestId}`);
        notification.close();
      };
      notification.onerror = () => setError(true);
    } catch {
      setError(true);
      setPermission(notificationPermission());
    }
  };

  const connected = useGameNotifications(
    contestId,
    viewerId,
    true,
    async (message, signal) => {
      setMessages((previous) =>
        previous.some((m) => m.key === message.key)
          ? previous
          : [...previous, message],
      );
      if (!enabled || notificationPermission() !== 'granted') return;
      if (document.visibilityState === 'visible' && document.hasFocus()) return;
      const key = `codelink:${viewerId}:${contestId}:${message.key}`;
      try {
        if ((await claimDesktopNotification(key)) && !signal.aborted) {
          const { title, body } = gameNotificationText(
            message.game,
            viewerId,
            t,
          );
          showDesktop(title, body, key);
        }
      } catch {
        setError(true);
      }
    },
  );

  const toggle = async () => {
    setError(false);
    if (enabled && permission === 'granted') {
      setEnabled(false);
      writeStorage('localStorage', preferenceKey, 'false');
      return;
    }
    try {
      const result = await Notification.requestPermission();
      setPermission(result);
      if (result === 'granted') {
        setEnabled(true);
        writeStorage('localStorage', preferenceKey, 'true');
      }
    } catch {
      setError(true);
    }
  };
  const active = enabled && permission === 'granted';
  const status =
    permission === 'unsupported'
      ? 'unsupported'
      : permission === 'denied'
        ? 'denied'
        : active
          ? 'enabled'
          : 'hint';

  return (
    <>
      <aside className="mx-auto flex max-w-7xl flex-wrap items-center gap-2 px-4 py-2 text-sm">
        <Bell className="h-4 w-4 shrink-0 text-primary" />
        <span className="text-muted-foreground">
          {t(`codelink-bracket.notifications.${status}`)}
        </span>
        {permission !== 'unsupported' && permission !== 'denied' && (
          <Button size="sm" variant="outline" onClick={() => void toggle()}>
            {t(
              `codelink-bracket.notifications.${active ? 'disable' : 'enable'}`,
            )}
          </Button>
        )}
        {active && (
          <Button
            size="sm"
            variant="ghost"
            onClick={() => {
              setError(false);
              showDesktop(
                t('codelink-bracket.notifications.testTitle'),
                t('codelink-bracket.notifications.testBody'),
                `codelink:test:${viewerId}`,
              );
            }}
          >
            {t('codelink-bracket.notifications.test')}
          </Button>
        )}
        {!connected && (
          <span className="text-amber-700 dark:text-amber-300">
            {t('codelink-bracket.notifications.reconnecting')}
          </span>
        )}
        {error && (
          <span role="status" className="text-destructive">
            {t('codelink-bracket.notifications.error')}
          </span>
        )}
      </aside>
      <div
        aria-live="polite"
        aria-relevant="additions"
        className="fixed bottom-4 right-4 z-50 max-h-[50vh] w-[calc(100%-2rem)] max-w-md space-y-3 overflow-y-auto"
      >
        {messages.map((message) => {
          const { title, body } = gameNotificationText(
            message.game,
            viewerId,
            t,
          );
          return (
            <section
              key={message.key}
              className="rounded-xl border-2 border-primary bg-card p-4 text-card-foreground shadow-xl"
            >
              <div className="flex items-start gap-2">
                <BellRing className="mt-0.5 h-5 w-5 shrink-0 text-primary" />
                <p className="flex-1 font-semibold">{title}</p>
                <Button
                  size="icon"
                  variant="ghost"
                  className="h-6 w-6 shrink-0"
                  aria-label={t('codelink-bracket.notifications.dismiss')}
                  onClick={() =>
                    setMessages((previous) =>
                      previous.filter((m) => m.key !== message.key),
                    )
                  }
                >
                  <X className="h-4 w-4" />
                </Button>
              </div>
              <p className="mt-2 text-sm">{body}</p>
              <Button asChild size="sm" variant="outline" className="mt-3">
                <Link to={`/contests/${contestId}`}>
                  {t('codelink-bracket.notifications.view')}
                </Link>
              </Button>
            </section>
          );
        })}
      </div>
    </>
  );
}
