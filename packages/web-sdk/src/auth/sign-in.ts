import { createContext, use, useCallback } from 'react';

/**
 * `next` if it is a same-origin path, else `/`. A return target is only ever
 * a path on this site, so a crafted `?next=https://evil.example` cannot turn
 * signing in into an open redirect.
 */
export function safeReturnPath(next: string | null | undefined): string {
  return next && next.startsWith('/') && !next.startsWith('//') ? next : '/';
}

function withReturn(page: string, returnTo: string): string {
  const next = safeReturnPath(returnTo);
  return next === '/' || next.startsWith(page)
    ? page
    : `${page}?next=${encodeURIComponent(next)}`;
}

/** The sign-in page, returning to `returnTo` (a same-origin path) after. */
export function signInPath(returnTo: string): string {
  return withReturn('/login', returnTo);
}

/** The registration page, returning to `returnTo` after. */
export function registerPath(returnTo: string): string {
  return withReturn('/register', returnTo);
}

/** The path, query and hash of the page being shown. */
export function currentPath(): string {
  if (typeof window === 'undefined') return '/';
  const { pathname, search, hash } = window.location;
  return `${pathname}${search}${hash}`;
}

/**
 * How the app goes to a path. The host provides its router's navigation;
 * without a provider the page is loaded afresh.
 */
export const NavigateContext = createContext<(path: string) => void>((path) =>
  window.location.assign(path),
);

/** Go to the sign-in page, coming back to the current page afterwards. */
export function useSignIn(): () => void {
  const navigate = use(NavigateContext);
  return useCallback(() => navigate(signInPath(currentPath())), [navigate]);
}
