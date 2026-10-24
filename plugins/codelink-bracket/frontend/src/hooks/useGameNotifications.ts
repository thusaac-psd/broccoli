import { useApiFetch } from '@broccoli/web-sdk/api';
import { useQueryClient } from '@tanstack/react-query';
import { useEffect, useRef, useState } from 'react';

import { readEventStream } from '../lib/event-stream';
import {
  type GameNotification,
  gameUpdates,
  readStorage,
  writeStorage,
} from '../lib/notifications';
import type { BracketResponse } from '../types';
import { PLUGIN_BASE } from './useBracketApi';

export function useGameNotifications(
  contestId: number | undefined,
  viewerId: number | undefined,
  onNotification: (
    notification: GameNotification,
    signal: AbortSignal,
  ) => void | Promise<void>,
): boolean {
  const apiFetch = useApiFetch();
  const queryClient = useQueryClient();
  const callback = useRef(onNotification);
  useEffect(() => {
    callback.current = onNotification;
  }, [onNotification]);
  const [connected, setConnected] = useState(false);

  useEffect(() => {
    if (!contestId || !viewerId) return;
    const controller = new AbortController();
    const { signal } = controller;
    const seenKey = `codelink:seen-games:${viewerId}:${contestId}`;
    let seen: string[] | null = null;
    try {
      const saved: unknown = JSON.parse(
        readStorage('sessionStorage', seenKey) ?? 'null',
      );
      if (Array.isArray(saved) && saved.every((key) => typeof key === 'string'))
        seen = saved;
    } catch {
      /* A missing/invalid baseline starts with the current games. */
    }
    let retryMs = 1_000;

    const run = async () => {
      while (!signal.aborted) {
        const started = Date.now();
        try {
          const response = await apiFetch(
            `${PLUGIN_BASE}/contests/${contestId}/bracket`,
            { signal, headers: { Accept: 'text/event-stream' } },
          );
          if ([401, 403, 404].includes(response.status)) return;
          if (
            !response.ok ||
            !response.body ||
            !response.headers
              .get('content-type')
              ?.startsWith('text/event-stream')
          ) {
            throw new Error('Notification stream unavailable');
          }
          setConnected(true);
          await readEventStream(response.body, async (event) => {
            if (signal.aborted || event.event !== 'snapshot') return;
            const snapshot = JSON.parse(event.data) as BracketResponse;
            const update = gameUpdates(snapshot.matches, viewerId, seen);
            for (const notification of update.notifications) {
              if (signal.aborted) return;
              await callback.current(notification, signal);
            }
            seen = update.seen;
            writeStorage('sessionStorage', seenKey, JSON.stringify(seen));
            if (update.notifications.length) {
              void queryClient.invalidateQueries({
                queryKey: ['codelink-bracket-bracket', contestId],
              });
            }
          });
          // The server regularly closes healthy streams for reauthorization.
          // Reconnect directly: a background setTimeout can be throttled for a minute.
          if (Date.now() - started >= 1_000) {
            retryMs = 1_000;
            continue;
          }
        } catch {
          if (signal.aborted) return;
        } finally {
          if (!signal.aborted) setConnected(false);
        }
        await new Promise<void>((resolve) => {
          const finish = () => {
            clearTimeout(timer);
            signal.removeEventListener('abort', finish);
            window.removeEventListener('online', finish);
            resolve();
          };
          const timer = setTimeout(finish, retryMs);
          signal.addEventListener('abort', finish, { once: true });
          window.addEventListener('online', finish, { once: true });
          if (signal.aborted) finish();
        });
        retryMs = Math.min(retryMs * 2, 15_000);
      }
    };
    void run();
    return () => controller.abort();
  }, [apiFetch, contestId, queryClient, viewerId]);

  return connected;
}
