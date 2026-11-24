import { errorKind, RequestError, usePluginApi } from '@broccoli/web-sdk/api';
import { useAuth, viewerCacheKey } from '@broccoli/web-sdk/auth';
import { nextPollDelay } from '@broccoli/web-sdk/hooks';
import { useQueryClient } from '@tanstack/react-query';
import { useEffect, useRef, useState } from 'react';

import { readEventStream } from '../lib/event-stream';
import { liveStreamKey, setLiveStream } from '../lib/live-stream';
import {
  type GameNotification,
  gameUpdates,
  readStorage,
  writeStorage,
} from '../lib/notifications';
import type { BracketResponse } from '../types';
import { PLUGIN_ID, ROUTES } from './useBracketApi';

/**
 * How soon a stream that ended or failed is opened again, and how long a
 * stream must have lived to count as healthy rather than as a failure.
 */
const RECONNECT_MS = 1_000;

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
    // Reconnects that failed in a row, and why the last one did: the wait
    // before the next is the shared poll backoff from `RECONNECT_MS`
    // (`nextPollDelay`), jittered and at least a `Retry-After`.
    let failures = 0;
    let lastError: unknown;
    const streamKey = liveStreamKey(contestId, viewerKey);
    const connect = (on: boolean) => {
      setConnected(on);
      setLiveStream(streamKey, on);
    };

    const run = async () => {
      while (!signal.aborted) {
        const started = performance.now();
        try {
          const response = await api.fetch(
            `${ROUTES}/contests/${contestId}/bracket`,
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
            throw RequestError.fromResponse(response, null);
          }
          connect(true);
          await readEventStream(response.body, async (event) => {
            if (signal.aborted || event.event !== 'snapshot') return;
            const snapshot = JSON.parse(event.data) as BracketResponse;
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
            const update = gameUpdates(snapshot.matches, viewerId, seen);
            for (const notification of update.notifications) {
              if (signal.aborted) return;
              await callback.current(notification, signal);
            }
            seen = update.seen;
            writeStorage('sessionStorage', seenKey, JSON.stringify(seen));
          });
          // The server regularly closes healthy streams for reauthorization.
          // Reconnect directly: a background setTimeout can be throttled for a minute.
          if (performance.now() - started >= RECONNECT_MS) {
            failures = 0;
            continue;
          }
          lastError = undefined;
        } catch (error) {
          if (signal.aborted) return;
          lastError = error;
        } finally {
          if (!signal.aborted) connect(false);
        }
        // Coming back online ends the wait only after a failure that was the
        // network's: a wait the server asked for (`Retry-After`) stands.
        const wakesOnline = errorKind(lastError) === 'network';
        await new Promise<void>((resolve) => {
          const finish = () => {
            clearTimeout(timer);
            signal.removeEventListener('abort', finish);
            window.removeEventListener('online', finish);
            resolve();
          };
          failures += 1;
          const timer = setTimeout(
            finish,
            nextPollDelay(RECONNECT_MS, failures, { error: lastError }),
          );
          signal.addEventListener('abort', finish, { once: true });
          if (wakesOnline) {
            window.addEventListener('online', finish, { once: true });
          }
          if (signal.aborted) finish();
        });
      }
    };
    void run();
    return () => {
      controller.abort();
      setLiveStream(streamKey, false);
    };
  }, [api, contestId, queryClient, viewerId, viewerKey]);

  return connected;
}
