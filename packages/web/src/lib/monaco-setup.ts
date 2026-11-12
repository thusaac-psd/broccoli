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
import { retryImport } from '@broccoli/web-sdk/utils';
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

// Monaco loads each language's tokenizer and configuration from its own chunk
// the first time the language is used, and remembers a failed load for good:
// one dropped request would leave that language unhighlighted until a page
// reload. So every lazily loaded language gets a loader that retries (see
// `retryImport`) and, should every retry fail, forgets the failure so the next
// use of the language tries again.
interface LazyLanguage {
  language: monaco.languages.IMonarchLanguage;
  conf: monaco.languages.LanguageConfiguration;
}

function retryingLanguageLoader(
  load: () => Promise<LazyLanguage>,
): () => Promise<LazyLanguage> {
  let loaded: Promise<LazyLanguage> | null = null;
  return () => {
    loaded ??= retryImport(load).catch((error: unknown) => {
      loaded = null;
      throw error;
    });
    return loaded;
  };
}

function registerTokens(id: string, load: () => Promise<LazyLanguage>) {
  monaco.languages.registerTokensProviderFactory(id, {
    create: async () => {
      try {
        return (await load()).language;
      } catch (error) {
        // Monaco keeps a factory's failure; a fresh one asks again next time.
        registerTokens(id, load);
        throw error;
      }
    },
  });
}

for (const language of monaco.languages.getLanguages()) {
  const lazy = (language as { loader?: () => Promise<LazyLanguage> }).loader;
  if (!lazy) continue;
  const load = retryingLanguageLoader(lazy);
  // Replaces Monaco's own factory, which would cache a failed load.
  registerTokens(language.id, load);
  monaco.languages.onLanguageEncountered(language.id, () => {
    load().then(
      ({ conf }) =>
        monaco.languages.setLanguageConfiguration(language.id, conf),
      (error) =>
        console.error(`Failed to load language ${language.id}:`, error),
    );
  });
}
