import { useEffect, useRef } from 'react';

/** Called with the id of a submission whose visibility changed. */
export type SubmissionVisibilityListener = (submissionId: number) => void;

const listeners = new Set<SubmissionVisibilityListener>();

/**
 * Says that what this viewer may see of a submission changed: a plugin
 * action unlocked or hid its outcome (a token, say). Every view of the
 * submission reads it again, including views that stopped polling because
 * the outcome was hidden. A plugin calls this after any action that changes
 * a submission's visibility; the host owns the views and how they refresh.
 */
export function announceSubmissionVisibilityChange(submissionId: number): void {
  for (const listener of [...listeners]) listener(submissionId);
}

/**
 * Calls `listener` with each submission whose visibility changes
 * (`announceSubmissionVisibilityChange`) until the returned function is
 * called.
 */
export function onSubmissionVisibilityChange(
  listener: SubmissionVisibilityListener,
): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
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
