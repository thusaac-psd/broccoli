import { useEffect, useRef } from 'react';

/** Called with the id of a submission whose visibility changed. */
export type SubmissionVisibilityListener = (submissionId: number) => void;

const listeners = new Set<SubmissionVisibilityListener>();

/**
 * The channel the app's tabs share: another tab may hold a view of the same
 * submission, one that stopped polling because the outcome was hidden.
 */
const CHANNEL = 'broccoli:submission-visibility';

/** This tab's end of the channel, open while anything here listens. */
let channel: BroadcastChannel | null = null;

function notify(submissionId: number): void {
  for (const listener of [...listeners]) listener(submissionId);
}

/**
 * Says that what this viewer may see of a submission changed: a plugin
 * action unlocked or hid its outcome (a token, say). Every view of the
 * submission reads it again, including views that stopped polling because
 * the outcome was hidden, in every tab of the app. A plugin calls this after any action that changes
 * a submission's visibility; the host owns the views and how they refresh.
 */
export function announceSubmissionVisibilityChange(submissionId: number): void {
  notify(submissionId);
  if (typeof BroadcastChannel === 'undefined') return;
  if (channel) {
    channel.postMessage(submissionId);
  } else {
    const once = new BroadcastChannel(CHANNEL);
    once.postMessage(submissionId);
    once.close();
  }
}

/**
 * Calls `listener` with each submission whose visibility changes
 * (`announceSubmissionVisibilityChange`), in this tab or another tab of the
 * app, until the returned function is called.
 */
export function onSubmissionVisibilityChange(
  listener: SubmissionVisibilityListener,
): () => void {
  listeners.add(listener);
  if (!channel && typeof BroadcastChannel !== 'undefined') {
    channel = new BroadcastChannel(CHANNEL);
    channel.onmessage = (event: MessageEvent<unknown>) => {
      if (typeof event.data === 'number') notify(event.data);
    };
  }
  return () => {
    listeners.delete(listener);
    if (listeners.size === 0 && channel) {
      channel.close();
      channel = null;
    }
  };
}

/**
 * Calls `listener` with each submission whose visibility changes while the
 * component is mounted, always the latest `listener` passed.
 */
export function useSubmissionVisibilityChange(
  listener: SubmissionVisibilityListener,
): void {
  const latest = useRef(listener);
  useEffect(() => {
    latest.current = listener;
  });
  useEffect(() => onSubmissionVisibilityChange((id) => latest.current(id)), []);
}
