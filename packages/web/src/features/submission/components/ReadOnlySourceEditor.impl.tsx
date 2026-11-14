import '@/lib/monaco-setup';

import Editor from '@monaco-editor/react';

interface ReadOnlySourceEditorProps {
  language: string;
  value: string;
  dark: boolean;
}

/** A read-only Monaco view of one source file. */
export function ReadOnlySourceEditor({
  language,
  value,
  dark,
}: ReadOnlySourceEditorProps) {
  return (
    <Editor
      height="100%"
      language={language}
      value={value}
      theme={dark ? 'vs-dark' : 'vs'}
      options={{
        readOnly: true,
        domReadOnly: true,
        minimap: { enabled: false },
        fontSize: 13,
        lineNumbers: 'on',
        scrollBeyondLastLine: false,
        renderLineHighlight: 'none',
        overviewRulerLanes: 0,
        hideCursorInOverviewRuler: true,
        overviewRulerBorder: false,
        scrollbar: { vertical: 'auto', horizontal: 'auto' },
        contextmenu: false,
        selectionHighlight: false,
        occurrencesHighlight: 'off',
        folding: false,
        lineDecorationsWidth: 8,
        padding: { top: 8, bottom: 8 },
      }}
    />
  );
}
