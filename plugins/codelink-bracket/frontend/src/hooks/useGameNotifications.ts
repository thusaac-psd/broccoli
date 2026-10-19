import { readEventStream, useApiFetch } from '@broccoli/web-sdk/api';
import { useQueryClient } from '@tanstack/react-query';
import { useEffect, useRef, useState } from 'react';

import {
  type GameNotification,
  isCursor,
  parseGameNotification,
  readStorage,
  writeStorage,
} from '../lib/notifications';

export function useGameNotifications(
  contestId: number | undefined,
  viewerId: number | undefined,
  enabled: boolean,
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
    if (!enabled || !contestId || !viewerId) return;
    const controller = new AbortController();
    const { signal } = controller;
    const cursorKey = `codelink:cursor:${viewerId}:${contestId}`;
    const stored = readStorage('sessionStorage', cursorKey);
    let cursor = isCursor(stored) ? stored : undefined;
    let retryMs = 1_000;

    const run = async () => {
      while (!signal.aborted) {
        const started = Date.now();
        try {
          const query = new URLSearchParams({ plugin_id: 'codelink-bracket' });
          if (cursor !== undefined) query.set('after', cursor);
          const response = await apiFetch(
            `/api/v1/contests/${contestId}/notifications?${query}`,
            { signal },
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
            if (signal.aborted || !isCursor(event.id)) return;
            if (event.event === 'ready') {
              cursor = event.id;
              writeStorage('sessionStorage', cursorKey, cursor);
              return;
            }
            if (
              event.event !== 'notification' ||
              (cursor !== undefined && BigInt(event.id) <= BigInt(cursor))
            )
              return;
            const notification = parseGameNotification(event.data, viewerId);
            if (notification) {
              await callback.current(notification, signal);
              void queryClient.invalidateQueries({
                queryKey: ['codelink-bracket-bracket', contestId],
              });
            }
            cursor = event.id;
            writeStorage('sessionStorage', cursorKey, cursor);
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
  }, [apiFetch, contestId, enabled, queryClient, viewerId]);

  return enabled && connected;
}
