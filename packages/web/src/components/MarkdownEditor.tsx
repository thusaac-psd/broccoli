import '@/lib/monaco-setup';

import { useTheme } from '@broccoli/web-sdk/theme';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@broccoli/web-sdk/ui';
import type { Monaco } from '@monaco-editor/react';
import Editor from '@monaco-editor/react';
import type { editor } from 'monaco-editor';
import { useCallback, useRef, useState } from 'react';

import { KeybindingModeDropdown } from '@/components/KeybindingModeDropdown';
import { Markdown } from '@/components/Markdown';
import { useEditorKeybindings } from '@/lib/use-editor-keybindings';

export interface MarkdownEditorProps {
  id?: string;
  value: string;
  onChange: (value: string) => void;
  minHeight?: number;
  placeholder?: string;
  onEditorMount?: (
    editor: editor.IStandaloneCodeEditor,
    monaco: Monaco,
  ) => void;
}

export function MarkdownEditor({
  id,
  value,
  onChange,
  minHeight = 200,
  placeholder,
  onEditorMount,
}: MarkdownEditorProps) {
  const { theme } = useTheme();
  const [tab, setTab] = useState<string>('write');
  const [editorInstance, setEditorInstance] =
    useState<editor.IStandaloneCodeEditor | null>(null);
  const [monacoInstance, setMonacoInstance] = useState<Monaco | null>(null);
  const vimStatusRef = useRef<HTMLDivElement | null>(null);
  const [keybindingMode, setKeybindingMode] = useEditorKeybindings(
    editorInstance,
    monacoInstance,
    vimStatusRef,
  );

  const handleMount = useCallback(
    (ed: editor.IStandaloneCodeEditor, monaco: Monaco) => {
      setEditorInstance(ed);
      setMonacoInstance(monaco);
      onEditorMount?.(ed, monaco);
    },
    [onEditorMount],
  );

  return (
    <Tabs value={tab} onValueChange={setTab}>
      <div className="flex items-center justify-between">
        <TabsList className="h-8">
          <TabsTrigger value="write" className="text-xs px-3 py-1">
            Write
          </TabsTrigger>
          <TabsTrigger value="preview" className="text-xs px-3 py-1">
            Preview
          </TabsTrigger>
        </TabsList>
        <KeybindingModeDropdown
          mode={keybindingMode}
          onChange={setKeybindingMode}
          compact
        />
      </div>
      <TabsContent value="write" className="mt-1">
        <div
          className="border rounded-md overflow-hidden"
          style={{ minHeight }}
        >
          <Editor
            height={`${minHeight}px`}
            language="markdown"
            value={value}
            onChange={(v) => onChange(v ?? '')}
            onMount={handleMount}
            theme={theme === 'dark' ? 'vs-dark' : 'vs'}
            options={{
              minimap: { enabled: false },
              fontSize: 13,
              lineNumbers: 'off',
              scrollBeyondLastLine: false,
              automaticLayout: true,
              wordWrap: 'on',
              tabSize: 2,
              renderLineHighlight: 'none',
              overviewRulerLanes: 0,
              hideCursorInOverviewRuler: true,
              overviewRulerBorder: false,
              scrollbar: { vertical: 'auto', horizontal: 'hidden' },
              padding: { top: 8, bottom: 8 },
              placeholder,
            }}
          />
          <div
            ref={vimStatusRef}
            className={`vim-status-bar h-6 border-t border-border bg-muted/40 px-3 font-mono text-[11px] leading-6 text-muted-foreground ${keybindingMode !== 'vim' ? 'hidden' : ''}`}
          />
        </div>
      </TabsContent>
      <TabsContent value="preview" className="mt-1">
        <div
          className="border rounded-md p-4 overflow-y-auto"
          style={{ minHeight }}
        >
          {value.trim() ? (
            <Markdown>{value}</Markdown>
          ) : (
            <p className="text-sm text-muted-foreground italic">
              Nothing to preview
            </p>
          )}
        </div>
      </TabsContent>
      {id && <input type="hidden" id={id} />}
    </Tabs>
  );
}
