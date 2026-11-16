import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Button } from '@broccoli/web-sdk/ui';
import { formatBytes } from '@broccoli/web-sdk/utils';
import type { Monaco } from '@monaco-editor/react';
import { ImagePlus, Paperclip } from 'lucide-react';
import type { editor, IDisposable, Position } from 'monaco-editor';
import { useCallback, useEffect, useRef, useState } from 'react';

import {
  MarkdownEditor,
  type MarkdownEditorProps,
} from '@/components/MarkdownEditor';
import {
  ATTACHMENT_DRAG_MIME,
  AttachmentPicker,
} from '@/features/admin/components/AttachmentPicker';
import {
  type Attachment,
  attachmentMarkdownRef,
  attachmentUrl,
  isImageType,
} from '@/features/problem/api/attachments';
import { useAttachmentUpload } from '@/features/problem/hooks/useAttachmentUpload';

interface Props extends Omit<MarkdownEditorProps, 'onEditorMount'> {
  problemId: number;
  attachments: Attachment[];
}

export function MarkdownEditorWithAttachments({
  problemId,
  attachments,
  ...editorProps
}: Props) {
  const { t } = useTranslation();
  const { upload } = useAttachmentUpload(problemId);

  const editorRef = useRef<editor.IStandaloneCodeEditor | null>(null);
  const monacoRef = useRef<Monaco | null>(null);
  const attachmentsRef = useRef<Attachment[]>(attachments);
  const disposablesRef = useRef<IDisposable[]>([]);

  const imageInputRef = useRef<HTMLInputElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);

  const [isDragOver, setIsDragOver] = useState(false);
  const dragCounter = useRef(0);

  // Keep attachments ref in sync
  useEffect(() => {
    attachmentsRef.current = attachments;
  }, [attachments]);

  // Cleanup disposables on unmount
  useEffect(() => {
    return () => {
      for (const d of disposablesRef.current) d.dispose();
    };
  }, []);

  // --- Upload + insert flow ---

  const insertPlaceholder = useCallback(
    (name: string, isImage: boolean): string => {
      const ed = editorRef.current;
      if (!ed) return '';

      const placeholderId = `uploading-${Math.random().toString(36).slice(2, 10)}`;
      const text = isImage
        ? `![Uploading ${name}...](${placeholderId})`
        : `[Uploading ${name}...](${placeholderId})`;

      const position = ed.getPosition();
      if (position) {
        ed.executeEdits('attachment-insert', [
          {
            range: {
              startLineNumber: position.lineNumber,
              startColumn: position.column,
              endLineNumber: position.lineNumber,
              endColumn: position.column,
            },
            text: text + '\n',
          },
        ]);
      }

      return placeholderId;
    },
    [],
  );

  const replacePlaceholder = useCallback(
    (placeholderId: string, replacement: string) => {
      const ed = editorRef.current;
      if (!ed) return;

      const model = ed.getModel();
      if (!model) return;

      const matches = model.findMatches(
        placeholderId,
        true,
        false,
        true,
        null,
        false,
      );

      if (matches.length === 0) return;

      // Find the full markdown expression containing the placeholder
      for (const match of matches) {
        const line = model.getLineContent(match.range.startLineNumber);
        const escaped = placeholderId.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
        const imgPattern = new RegExp(
          `!\\[Uploading [^\\]]*\\]\\(${escaped}\\)`,
        );
        const linkPattern = new RegExp(
          `\\[Uploading [^\\]]*\\]\\(${escaped}\\)`,
        );

        const imgMatch = imgPattern.exec(line);
        const linkMatch = linkPattern.exec(line);
        const found = imgMatch ?? linkMatch;

        if (found) {
          const startCol = found.index + 1; // Monaco is 1-based
          const endCol = startCol + found[0].length;
          ed.executeEdits('attachment-replace', [
            {
              range: {
                startLineNumber: match.range.startLineNumber,
                startColumn: startCol,
                endLineNumber: match.range.startLineNumber,
                endColumn: endCol,
              },
              text: replacement,
            },
          ]);
          return;
        }
      }
    },
    [],
  );

  const removePlaceholder = useCallback((placeholderId: string) => {
    const ed = editorRef.current;
    if (!ed) return;

    const model = ed.getModel();
    if (!model) return;

    const matches = model.findMatches(
      placeholderId,
      true,
      false,
      true,
      null,
      false,
    );

    if (matches.length === 0) return;

    for (const match of matches) {
      const lineNum = match.range.startLineNumber;
      const line = model.getLineContent(lineNum);
      const escaped = placeholderId.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
      const fullPattern = new RegExp(
        `!?\\[Uploading [^\\]]*\\]\\(${escaped}\\)`,
      );
      const found = fullPattern.exec(line);

      if (found) {
        const remainingText =
          line.slice(0, found.index) +
          line.slice(found.index + found[0].length);
        // If the line becomes empty after removing the placeholder, delete the whole line
        if (remainingText.trim() === '' && model.getLineCount() > 1) {
          const endLine = lineNum + 1;
          ed.executeEdits('attachment-remove', [
            {
              range: {
                startLineNumber: lineNum,
                startColumn: 1,
                endLineNumber: endLine,
                endColumn: 1,
              },
              text: '',
            },
          ]);
        } else {
          const startCol = found.index + 1;
          const endCol = startCol + found[0].length;
          ed.executeEdits('attachment-remove', [
            {
              range: {
                startLineNumber: lineNum,
                startColumn: startCol,
                endLineNumber: lineNum,
                endColumn: endCol,
              },
              text: '',
            },
          ]);
        }
        return;
      }
    }
  }, []);

  const insertAtCursor = useCallback((text: string) => {
    const ed = editorRef.current;
    if (!ed) return;
    const position = ed.getPosition();
    if (!position) return;
    ed.executeEdits('attachment-insert', [
      {
        range: {
          startLineNumber: position.lineNumber,
          startColumn: position.column,
          endLineNumber: position.lineNumber,
          endColumn: position.column,
        },
        text: text + '\n',
      },
    ]);
    ed.focus();
  }, []);

  const uploadAndInsert = useCallback(
    async (file: File) => {
      const isImage = file.type.startsWith('image/');
      const placeholderId = insertPlaceholder(file.name, isImage);
      if (!placeholderId) return;

      const attachment = await upload(file);
      if (attachment) {
        const url = attachmentUrl(problemId, attachment.id);
        const md = attachmentMarkdownRef(
          attachment.path,
          url,
          isImageType(attachment.content_type),
        );
        replacePlaceholder(placeholderId, md);
      } else {
        removePlaceholder(placeholderId);
      }
    },
    [
      upload,
      problemId,
      insertPlaceholder,
      replacePlaceholder,
      removePlaceholder,
    ],
  );

  const handleDragEnter = useCallback((e: React.DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    dragCounter.current++;
    if (dragCounter.current === 1) {
      setIsDragOver(true);
    }
  }, []);

  const handleDragLeave = useCallback((e: React.DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    dragCounter.current--;
    if (dragCounter.current === 0) {
      setIsDragOver(false);
    }
  }, []);

  const handleDragOver = useCallback((e: React.DragEvent) => {
    e.preventDefault();
    e.dataTransfer.dropEffect = 'copy';
  }, []);

  const handleDrop = useCallback(
    (e: React.DragEvent) => {
      e.preventDefault();
      e.stopPropagation();
      dragCounter.current = 0;
      setIsDragOver(false);

      // Check for internal attachment drag from picker (no upload needed)
      const attachmentMd = e.dataTransfer.getData(ATTACHMENT_DRAG_MIME);
      if (attachmentMd) {
        insertAtCursor(attachmentMd);
        return;
      }

      const files = Array.from(e.dataTransfer.files);
      if (files.length === 0) return;

      // Intentionally parallel - each file gets its own placeholder and uploads concurrently
      for (const file of files) {
        uploadAndInsert(file);
      }
    },
    [uploadAndInsert, insertAtCursor],
  );

  const handleImageInputChange = useCallback(
    (e: React.ChangeEvent<HTMLInputElement>) => {
      const files = Array.from(e.target.files ?? []);
      for (const file of files) uploadAndInsert(file);
      e.target.value = '';
    },
    [uploadAndInsert],
  );

  const handleFileInputChange = useCallback(
    (e: React.ChangeEvent<HTMLInputElement>) => {
      const files = Array.from(e.target.files ?? []);
      for (const file of files) uploadAndInsert(file);
      e.target.value = '';
    },
    [uploadAndInsert],
  );

  const handleEditorMount = useCallback(
    (ed: editor.IStandaloneCodeEditor, monaco: Monaco) => {
      editorRef.current = ed;
      monacoRef.current = monaco;

      for (const d of disposablesRef.current) d.dispose();
      disposablesRef.current = [];

      // Paste handler
      const domNode = ed.getDomNode();
      if (domNode) {
        const pasteHandler = (e: ClipboardEvent) => {
          const items = e.clipboardData?.items;
          if (!items) return;

          for (const item of Array.from(items)) {
            if (item.type.startsWith('image/')) {
              e.preventDefault();
              const file = item.getAsFile();
              if (file) uploadAndInsert(file);
              return;
            }
          }
        };
        domNode.addEventListener('paste', pasteHandler, { capture: true });
        disposablesRef.current.push({
          dispose: () =>
            domNode.removeEventListener('paste', pasteHandler, {
              capture: true,
            }),
        });
      }

      // Completion provider
      const completionProvider =
        monaco.languages.registerCompletionItemProvider('markdown', {
          triggerCharacters: ['[', '('],
          provideCompletionItems(model: editor.ITextModel, position: Position) {
            const lineContent = model.getLineContent(position.lineNumber);
            const textBefore = lineContent.substring(0, position.column - 1);

            const items: Attachment[] = attachmentsRef.current;
            if (items.length === 0) return { suggestions: [] };

            // Check if preceded by ![ (for full markdown insertion)
            const imgBracket = textBefore.lastIndexOf('![');

            // If we're in a ![ context, suggest full ![path](url)
            if (
              imgBracket >= 0 &&
              !textBefore.substring(imgBracket).includes(']')
            ) {
              return {
                suggestions: items.map((att) => {
                  const url = attachmentUrl(problemId, att.id);
                  const isImg = isImageType(att.content_type);
                  const md = attachmentMarkdownRef(att.path, url, isImg);
                  return {
                    label: att.path,
                    kind: monaco.languages.CompletionItemKind.File,
                    detail: formatBytes(att.size),
                    documentation: url,
                    insertText: md,
                    range: {
                      startLineNumber: position.lineNumber,
                      startColumn: imgBracket + 1, // replace from ![
                      endLineNumber: position.lineNumber,
                      endColumn: position.column,
                    },
                    sortText: att.path,
                  };
                }),
              };
            }

            // If we're in a ]( context, suggest just URLs
            const linkParen = textBefore.lastIndexOf('](');
            if (
              linkParen >= 0 &&
              !textBefore.substring(linkParen + 2).includes(')')
            ) {
              return {
                suggestions: items.map((att) => {
                  const url = attachmentUrl(problemId, att.id);
                  return {
                    label: att.path,
                    kind: monaco.languages.CompletionItemKind.File,
                    detail: formatBytes(att.size),
                    insertText: url,
                    range: {
                      startLineNumber: position.lineNumber,
                      startColumn: linkParen + 3, // after ](
                      endLineNumber: position.lineNumber,
                      endColumn: position.column,
                    },
                    sortText: att.path,
                  };
                }),
              };
            }

            return { suggestions: [] };
          },
        });
      disposablesRef.current.push(completionProvider);

      // Hover provider
      const hoverProvider = monaco.languages.registerHoverProvider('markdown', {
        provideHover(model: editor.ITextModel, position: Position) {
          const line = model.getLineContent(position.lineNumber);
          const pattern =
            /\/api\/v1\/problems\/\d+\/attachments\/([a-f0-9-]+)/g;
          let match: RegExpExecArray | null;

          while ((match = pattern.exec(line)) !== null) {
            const start = match.index + 1;
            const end = start + match[0].length;

            if (position.column >= start && position.column <= end) {
              const refId = match[1];
              const att = attachmentsRef.current.find((a) => a.id === refId);
              if (!att) return null;

              // Text-only hover - image preview would require auth headers which Monaco can't provide
              const contents: { value: string }[] = [
                {
                  value: `**${att.path}** (${formatBytes(att.size)}, ${att.content_type ?? 'unknown'})`,
                },
              ];

              return {
                range: {
                  startLineNumber: position.lineNumber,
                  startColumn: start,
                  endLineNumber: position.lineNumber,
                  endColumn: end,
                },
                contents,
              };
            }
          }

          return null;
        },
      });
      disposablesRef.current.push(hoverProvider);

      // Keyboard shortcuts
      ed.addCommand(
        monaco.KeyMod.CtrlCmd | monaco.KeyMod.Shift | monaco.KeyCode.KeyI,
        () => imageInputRef.current?.click(),
      );
      ed.addCommand(
        monaco.KeyMod.CtrlCmd | monaco.KeyMod.Shift | monaco.KeyCode.KeyK,
        () => fileInputRef.current?.click(),
      );
    },
    [uploadAndInsert, problemId],
  );

  return (
    <div
      className="relative"
      onDragEnter={handleDragEnter}
      onDragLeave={handleDragLeave}
      onDragOver={handleDragOver}
      onDrop={handleDrop}
    >
      {/* Toolbar */}
      <div className="flex items-center gap-1 mb-1">
        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="h-7 px-2 text-xs"
          onClick={() => imageInputRef.current?.click()}
          title={`${t('admin.attachments.insertImage')} (Ctrl+Shift+I)`}
        >
          <ImagePlus className="h-3.5 w-3.5 mr-1" />
          {t('admin.attachments.insertImage')}
        </Button>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="h-7 px-2 text-xs"
          onClick={() => fileInputRef.current?.click()}
          title={`${t('admin.attachments.insertFile')} (Ctrl+Shift+K)`}
        >
          <Paperclip className="h-3.5 w-3.5 mr-1" />
          {t('admin.attachments.insertFile')}
        </Button>

        <div className="h-4 w-px bg-border mx-0.5" />

        <AttachmentPicker
          problemId={problemId}
          attachments={attachments}
          onInsert={insertAtCursor}
        />
      </div>

      {/* Hidden file inputs */}
      <input
        ref={imageInputRef}
        type="file"
        accept="image/*"
        className="hidden"
        onChange={handleImageInputChange}
        multiple
      />
      <input
        ref={fileInputRef}
        type="file"
        className="hidden"
        onChange={handleFileInputChange}
        multiple
      />

      {/* Editor */}
      <MarkdownEditor {...editorProps} onEditorMount={handleEditorMount} />

      {/* Drag overlay */}
      {isDragOver && (
        <div className="absolute inset-0 z-10 flex items-center justify-center rounded-md border-2 border-dashed border-primary bg-primary/5 pointer-events-none">
          <div className="flex flex-col items-center gap-2 text-primary">
            <ImagePlus className="h-8 w-8" />
            <span className="text-sm font-medium">
              {t('admin.attachments.dropImageHint')}
            </span>
          </div>
        </div>
      )}
    </div>
  );
}
