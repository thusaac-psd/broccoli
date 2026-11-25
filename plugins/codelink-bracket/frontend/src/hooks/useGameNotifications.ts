import { usePluginApi } from '@broccoli/web-sdk/api';
import { useAuth, viewerCacheKey } from '@broccoli/web-sdk/auth';
import { nextPollDelay } from '@broccoli/web-sdk/hooks';
import { useQueryClient } from '@tanstack/react-query';
import { useEffect, useRef, useState } from 'react';

import { RECONNECT_MS, runGameStream } from '../lib/game-stream';
import { liveStreamKey, setLiveStream } from '../lib/live-stream';
import {
  type GameNotification,
  readStorage,
  writeStorage,
} from '../lib/notifications';
import { PLUGIN_ID, ROUTES } from './useBracketApi';

export function useGameNotifications(
  contestId: number | undefined,
  viewerId: number | undefined,
  onNotification: (
    notification: GameNotification,
    signal: AbortSignal,
  ) => void | Promise<void>,
): boolean {
  const api = usePluginApi(PLUGIN_ID);
  const queryClient = useQueryClient();
  const { user } = useAuth();
  const viewerKey = viewerCacheKey(user);
  const callback = useRef(onNotification);
  useEffect(() => {
    callback.current = onNotification;
  }, [onNotification]);
  const [connected, setConnected] = useState(false);

  useEffect(() => {
    if (!contestId || !viewerId) return;
    const controller = new AbortController();
    const seenKey = `codelink:seen-games:${viewerId}:${contestId}`;
    const streamKey = liveStreamKey(contestId, viewerKey);
    void runGameStream({
      open: (signal) =>
        api.fetch(`${ROUTES}/contests/${contestId}/bracket`, {
          signal,
          headers: { Accept: 'text/event-stream' },
        }),
      now: () => performance.now(),
      // A background setTimeout can be throttled for a minute, so a healthy
      // stream's reconnect never waits on one (`runGameStream`).
      wait: (ms, wakesOnline, signal) =>
        new Promise<void>((resolve) => {
          const finish = () => {
            clearTimeout(timer);
            signal.removeEventListener('abort', finish);
            window.removeEventListener('online', finish);
            resolve();
          };
          const timer = setTimeout(finish, ms);
          signal.addEventListener('abort', finish, { once: true });
          if (wakesOnline) {
            window.addEventListener('online', finish, { once: true });
          }
          if (signal.aborted) finish();
        }),
      // The shared poll backoff from `RECONNECT_MS`, jittered and at least a
      // `Retry-After`.
      retryDelay: (failures, error) =>
        nextPollDelay(RECONNECT_MS, failures, { error }),
      connected: (on) => {
        setConnected(on);
        setLiveStream(streamKey, on);
      },
      snapshot: (snapshot) => {
        queryClient.setQueryData(
          ['codelink-bracket-bracket', contestId, viewerKey],
          snapshot,
        );
        for (const match of snapshot.matches) {
          queryClient.setQueryData(
            ['codelink-bracket-match', contestId, match.id, viewerKey],
            match,
          );
        }
      },
      notify: (notification, signal) => callback.current(notification, signal),
      seen: {
        read: () => {
          try {
            const saved: unknown = JSON.parse(
              readStorage('sessionStorage', seenKey) ?? 'null',
            );
            if (
              Array.isArray(saved) &&
              saved.every((key) => typeof key === 'string')
            )
              return saved;
          } catch {
            /* A missing/invalid baseline starts with the current games. */
          }
          return null;
        },
        write: (seen) =>
          writeStorage('sessionStorage', seenKey, JSON.stringify(seen)),
      },
      viewerId,
      signal: controller.signal,
    });
    return () => {
      controller.abort();
      setLiveStream(streamKey, false);
    };
  }, [api, contestId, queryClient, viewerId, viewerKey]);

  return connected;
}
