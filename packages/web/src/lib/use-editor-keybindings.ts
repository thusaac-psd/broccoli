import { retryImport } from '@broccoli/web-sdk/utils';
import type * as monacoNs from 'monaco-editor';
import { type RefObject, useEffect, useRef } from 'react';

import { activateEmacsMode } from '@/lib/emacs-keybindings';
import {
  type KeybindingMode,
  useKeybindingMode,
} from '@/lib/use-keybinding-mode';

/**
 * Applies vim or emacs keybindings to a Monaco editor instance.
 *
 * @param editorInstance - The mounted Monaco editor (null before mount)
 * @param monacoInstance - The Monaco namespace (needed for KeyCode/KeyMod enums)
 * @param statusBarRef  - A ref to a DOM element for vim's status line
 * @returns [mode, setMode] - current keybinding mode and setter
 */
export function useEditorKeybindings(
  editorInstance: monacoNs.editor.IStandaloneCodeEditor | null,
  monacoInstance: typeof monacoNs | null,
  statusBarRef: RefObject<HTMLDivElement | null>,
): [KeybindingMode, (mode: KeybindingMode) => void] {
  const [mode, setMode] = useKeybindingMode();
  const adapterRef = useRef<{ dispose: () => void } | null>(null);

  useEffect(() => {
    adapterRef.current?.dispose();
    adapterRef.current = null;

    if (!editorInstance || !monacoInstance) return;

    let cancelled = false;

    if (mode === 'vim') {
      retryImport(() => import('monaco-vim')).then(
        ({ initVimMode }) => {
          if (cancelled) return;
          adapterRef.current = initVimMode(
            editorInstance,
            statusBarRef.current ?? undefined,
          );
        },
        (error) => console.error('Failed to load vim mode:', error),
      );
    } else if (mode === 'emacs') {
      adapterRef.current = activateEmacsMode(editorInstance, monacoInstance);
    }

    return () => {
      cancelled = true;
      adapterRef.current?.dispose();
      adapterRef.current = null;
    };
  }, [editorInstance, monacoInstance, mode, statusBarRef]);

  return [mode, setMode];
}
