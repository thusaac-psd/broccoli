import { retryImport } from '@broccoli/web-sdk/utils';
import { type ComponentType, lazy, type LazyExoticComponent } from 'react';

/**
 * A component whose code loads on first render, from a module reachable only
 * through this dynamic import (an `*.impl` module: see the import rules in
 * the root ESLint config). A failed load is retried like every lazy import.
 */
export function lazyImpl<
  // `React.lazy` itself takes components of any props.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  M extends Record<K, ComponentType<any>>,
  K extends keyof M,
>(load: () => Promise<M>, name: K): LazyExoticComponent<M[K]> {
  return lazy(() => retryImport(load).then((m) => ({ default: m[name] })));
}
