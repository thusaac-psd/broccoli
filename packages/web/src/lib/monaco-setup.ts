/**
 * Self-hosted Monaco bootstrap.
 *
 * `@monaco-editor/react` defaults to fetching Monaco's AMD build from
 * cdn.jsdelivr.net, which breaks on air-gapped contest networks. Handing the
 * loader the bundled ESM instance keeps the editor and its workers on our own
 * origin. Import this module (for its side effects) before rendering
 * `<Editor>`; the shared-deps shim for `@monaco-editor/react` imports it too, so
 * plugins get the same offline instance.
 */
import { loader } from '@monaco-editor/react';
import * as monaco from 'monaco-editor';
import EditorWorker from 'monaco-editor/esm/vs/editor/editor.worker?worker';
import CssWorker from 'monaco-editor/esm/vs/language/css/css.worker?worker';
import HtmlWorker from 'monaco-editor/esm/vs/language/html/html.worker?worker';
import JsonWorker from 'monaco-editor/esm/vs/language/json/json.worker?worker';
import TsWorker from 'monaco-editor/esm/vs/language/typescript/ts.worker?worker';

// Monaco's own ESM fallback bootstraps the editor worker from a blob URL,
// which Vite cannot rewrite; route every label to a Vite-bundled worker.
self.MonacoEnvironment = {
  getWorker(_workerId, label) {
    switch (label) {
      case 'json':
        return new JsonWorker();
      case 'css':
      case 'scss':
      case 'less':
        return new CssWorker();
      case 'html':
      case 'handlebars':
      case 'razor':
        return new HtmlWorker();
      case 'typescript':
      case 'javascript':
        return new TsWorker();
      default:
        return new EditorWorker();
    }
  },
};

loader.config({ monaco });
