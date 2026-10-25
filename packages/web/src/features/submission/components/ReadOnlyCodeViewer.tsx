import '@/lib/monaco-setup';

import { useTranslation } from '@broccoli/web-sdk/i18n';
import { useTheme } from '@broccoli/web-sdk/theme';
import { Badge } from '@broccoli/web-sdk/ui';
import { cn } from '@broccoli/web-sdk/utils';
import Editor from '@monaco-editor/react';
import { ChevronDown, FileText, GripHorizontal } from 'lucide-react';
import { useCallback, useRef, useState } from 'react';

const LANG_TO_MONACO: Record<string, string> = {
  cpp: 'cpp',
  c: 'c',
  python3: 'python',
  java: 'java',
  rust: 'rust',
  go: 'go',
  javascript: 'javascript',
  typescript: 'typescript',
};

const LANG_DISPLAY: Record<string, string> = {
  cpp: 'C++',
  c: 'C',
  python3: 'Python',
  java: 'Java',
  rust: 'Rust',
  go: 'Go',
  javascript: 'JS',
  typescript: 'TS',
};

interface ReadOnlyCodeViewerProps {
  files: { filename: string; content: string }[];
  language?: string;
  defaultOpen?: boolean;
}

export function ReadOnlyCodeViewer({
  files,
  language,
  defaultOpen = false,
}: ReadOnlyCodeViewerProps) {
  const [open, setOpen] = useState(defaultOpen);
  const [activeIndex, setActiveIndex] = useState(0);
  const [userHeight, setUserHeight] = useState<number | null>(null);
  const dragRef = useRef<{ startY: number; startH: number } | null>(null);
  const heightRef = useRef(0);
  const { theme } = useTheme();
  const { t } = useTranslation();

  const onPointerMove = useCallback((e: PointerEvent) => {
    if (!dragRef.current) return;
    const delta = e.clientY - dragRef.current.startY;
    setUserHeight(Math.max(80, dragRef.current.startH + delta));
  }, []);

  const onPointerUp = useCallback(() => {
    dragRef.current = null;
    document.removeEventListener('pointermove', onPointerMove);
    document.removeEventListener('pointerup', onPointerUp);
    document.body.style.userSelect = '';
  }, [onPointerMove]);

  const onDragStart = useCallback(
    (e: React.PointerEvent) => {
      dragRef.current = { startY: e.clientY, startH: heightRef.current };
      document.addEventListener('pointermove', onPointerMove);
      document.addEventListener('pointerup', onPointerUp);
      document.body.style.userSelect = 'none';
    },
    [onPointerMove, onPointerUp],
  );

  if (files.length === 0) return null;

  const activeFile = files[activeIndex] ?? files[0];
  const lineCount = activeFile.content.split('\n').length;
  const autoHeight = Math.min(Math.max(lineCount * 19, 80), 400);
  const editorHeight = userHeight ?? autoHeight;
  heightRef.current = editorHeight;
  const monacoLang = language
    ? (LANG_TO_MONACO[language] ?? language)
    : 'plaintext';
  const langLabel = language ? (LANG_DISPLAY[language] ?? language) : null;

  return (
    <div className="overflow-hidden rounded-lg border border-border">
      {/* Header */}
      <button
        type="button"
        onClick={() => setOpen(!open)}
        className={cn(
          'flex w-full cursor-pointer items-center justify-between gap-2 border-none px-3 py-2',
          'bg-muted/50 hover:bg-muted transition-colors',
        )}
      >
        <div className="flex min-w-0 items-center gap-2">
          <FileText className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
          <span className="truncate font-mono text-xs text-foreground">
            {t('submissionDetail.sourceCode')}
          </span>
          {langLabel && (
            <Badge variant="outline" className="px-1.5 py-0 text-[10px]">
              {langLabel}
            </Badge>
          )}
        </div>
        <ChevronDown
          className={cn(
            'h-4 w-4 shrink-0 text-muted-foreground transition-transform duration-200',
            open && 'rotate-180',
          )}
        />
      </button>

      {/* Collapsible body — CSS grid-template-rows animation */}
      <div
        className={cn(
          'grid transition-[grid-template-rows] duration-250 ease-in-out',
          open ? 'grid-rows-[1fr]' : 'grid-rows-[0fr]',
        )}
      >
        <div className="overflow-hidden">
          {/* Multi-file tabs */}
          {files.length > 1 && (
            <div className="flex gap-0 overflow-x-auto border-b border-border bg-muted/30">
              {files.map((file, i) => (
                <button
                  key={file.filename}
                  type="button"
                  onClick={(e) => {
                    e.stopPropagation();
                    setActiveIndex(i);
                  }}
                  className={cn(
                    'shrink-0 border-b-2 px-3 py-1.5 font-mono text-xs transition-colors',
                    i === activeIndex
                      ? 'border-primary text-foreground bg-background'
                      : 'border-transparent text-muted-foreground hover:text-foreground hover:bg-muted/50',
                  )}
                >
                  {file.filename}
                </button>
              ))}
            </div>
          )}

          {/* Monaco editor */}
          <div style={{ height: editorHeight }}>
            {open && (
              <Editor
                height="100%"
                language={monacoLang}
                value={activeFile.content}
                theme={theme === 'dark' ? 'vs-dark' : 'vs'}
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
            )}
          </div>

          {/* Resize handle */}
          <div
            onPointerDown={onDragStart}
            className={cn(
              'flex h-5 cursor-row-resize items-center justify-center',
              'border-t border-border bg-muted/30 hover:bg-muted/60 transition-colors',
            )}
          >
            <GripHorizontal className="h-3 w-3 text-muted-foreground" />
          </div>
        </div>
      </div>
    </div>
  );
}
