import { useAuth } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Button, toast } from '@broccoli/web-sdk/ui';
import { Bell } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import { useNavigate } from 'react-router';

import { useGameNotifications } from './hooks/useGameNotifications';
import { useMyMatch } from './hooks/useMyMatch';
import {
  claimDesktopNotification,
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
  const shown = useRef(new Set<string>());
  const [error, setError] = useState(false);

  useEffect(() => {
    const ids = shown.current;
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
      ids.forEach((id) => toast.dismiss(id));
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
    async (message, signal) => {
      const key = `codelink:${viewerId}:${contestId}:${message.key}`;
      const { title, body } = gameNotificationText(message, t);
      shown.current.add(key);
      toast.info(title, {
        id: key,
        description: body,
        duration: Infinity,
        closeButton: true,
        style: { borderColor: 'var(--primary)', borderWidth: 2 },
        action: {
          label: t('codelink-bracket.notifications.view'),
          onClick: () => navigate(`/contests/${contestId}`),
        },
      });
      if (!enabled || notificationPermission() !== 'granted') return;
      if (document.visibilityState === 'visible' && document.hasFocus()) return;
      try {
        if ((await claimDesktopNotification(key)) && !signal.aborted)
          showDesktop(title, body, key);
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
    <aside className="mx-auto flex max-w-7xl flex-wrap items-center gap-2 px-4 py-2 text-sm">
      <Bell className="h-4 w-4 shrink-0 text-primary" />
      <span className="text-muted-foreground">
        {t(`codelink-bracket.notifications.${status}`)}
      </span>
      {permission !== 'unsupported' && permission !== 'denied' && (
        <Button size="sm" variant="outline" onClick={() => void toggle()}>
          {t(`codelink-bracket.notifications.${active ? 'disable' : 'enable'}`)}
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
  );
}
