import { Skeleton } from '@broccoli/web-sdk/ui';
import { retryImport } from '@broccoli/web-sdk/utils';
import { lazy, Suspense } from 'react';

import type { MarkdownEditorProps } from './MarkdownEditor.impl';

export type { MarkdownEditorProps } from './MarkdownEditor.impl';

// Monaco is about a megabyte: only a form that shows the editor loads it.
// The implementation is reachable only through this dynamic import.
const Impl = lazy(() =>
  retryImport(() => import('./MarkdownEditor.impl')).then((m) => ({
    default: m.MarkdownEditor,
  })),
);

export function MarkdownEditor(props: MarkdownEditorProps) {
  return (
    <Suspense
      fallback={
        <Skeleton
          className="w-full"
          style={{ minHeight: props.minHeight ?? 200 }}
        />
      }
    >
      <Impl {...props} />
    </Suspense>
  );
}
