import { useSyncExternalStore } from 'react';

// Which (contest, viewer) bracket snapshots arrive over the live stream right
// now. While one does, the stream writes every snapshot into the bracket query
// (the server re-reads the bracket for it each second), so polling the same
// route over HTTP would only read it a second time.

const live = new Set<string>();
const listeners = new Set<() => void>();

/** The live stream's key for a contest's bracket as `viewerKey` sees it. */
export function liveStreamKey(contestId: number, viewerKey: string): string {
  return `${contestId}:${viewerKey}`;
}

/** Record whether the stream of `key` is connected. */
export function setLiveStream(key: string, connected: boolean): void {
  if (connected === live.has(key)) return;
  if (connected) live.add(key);
  else live.delete(key);
  listeners.forEach((listener) => listener());
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** Whether the stream of `key` is connected, re-rendering when that changes. */
export function useLiveStream(key: string | null): boolean {
  return useSyncExternalStore(
    subscribe,
    () => key !== null && live.has(key),
    () => false,
  );
}
