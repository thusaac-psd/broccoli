import { Skeleton } from '@broccoli/web-sdk/ui';
import { retryImport } from '@broccoli/web-sdk/utils';
import { lazy, Suspense } from 'react';

import type { CodeEditorProps } from './CodeEditor.impl';

export type { EditorFile } from './code-editor-draft';
export type { CodeEditorProps } from './CodeEditor.impl';

// Monaco is about a megabyte: only a page that shows an editor loads it.
// The implementation is reachable only through this dynamic import.
const Impl = lazy(() =>
  retryImport(() => import('./CodeEditor.impl')).then((m) => ({
    default: m.CodeEditor,
  })),
);

export function CodeEditor(props: CodeEditorProps) {
  return (
    <Suspense fallback={<Skeleton className="h-full min-h-64 w-full" />}>
      <Impl {...props} />
    </Suspense>
  );
}
