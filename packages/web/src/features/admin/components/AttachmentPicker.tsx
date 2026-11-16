import { useApiBasePath } from '@broccoli/web-sdk/api';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Popover, PopoverContent, PopoverTrigger } from '@broccoli/web-sdk/ui';
import { formatBytes } from '@broccoli/web-sdk/utils';
import {
  File as FileIcon,
  FileArchive,
  FileText,
  GripVertical,
  Images,
} from 'lucide-react';
import { useState } from 'react';

import { AuthImage } from '@/components/AuthImage';
import {
  type Attachment,
  attachmentMarkdownRef,
  attachmentUrl,
  isImageType,
} from '@/features/problem/api/attachments';

/** Custom MIME type used to identify internal attachment drags */
export const ATTACHMENT_DRAG_MIME = 'application/x-broccoli-attachment';

interface AttachmentPickerProps {
  problemId: number;
  attachments: Attachment[];
  onInsert: (markdown: string) => void;
}

function FileTypeIcon({ contentType }: { contentType: string | null }) {
  if (contentType?.startsWith('text/'))
    return <FileText className="h-5 w-5 text-muted-foreground" />;
  if (
    contentType?.includes('zip') ||
    contentType?.includes('archive') ||
    contentType?.includes('compressed')
  )
    return <FileArchive className="h-5 w-5 text-muted-foreground" />;
  return <FileIcon className="h-5 w-5 text-muted-foreground" />;
}

export function AttachmentPicker({
  problemId,
  attachments,
  onInsert,
}: AttachmentPickerProps) {
  const { t } = useTranslation();
  const apiBasePath = useApiBasePath();
  const [open, setOpen] = useState(false);

  function getMarkdown(att: Attachment) {
    const url = attachmentUrl(apiBasePath, problemId, att.id);
    return attachmentMarkdownRef(att.path, url, isImageType(att.content_type));
  }

  function handleClick(att: Attachment) {
    onInsert(getMarkdown(att));
    setOpen(false);
  }

  function handleDragStart(e: React.DragEvent, att: Attachment) {
    const md = getMarkdown(att);
    e.dataTransfer.setData(ATTACHMENT_DRAG_MIME, md);
    e.dataTransfer.setData('text/plain', md);
    e.dataTransfer.effectAllowed = 'copy';
  }

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <button
          type="button"
          className="inline-flex items-center gap-1 rounded-md px-2 h-7 text-xs font-medium text-muted-foreground hover:bg-accent hover:text-accent-foreground transition-colors"
          title={t('admin.attachments.pickerTitle')}
        >
          <Images className="h-3.5 w-3.5" />
          {t('admin.attachments.pickerTitle')}
          {attachments.length > 0 && (
            <span className="ml-0.5 text-[10px] rounded-full bg-muted px-1.5">
              {attachments.length}
            </span>
          )}
        </button>
      </PopoverTrigger>
      <PopoverContent
        align="start"
        className="w-72 p-0"
        onOpenAutoFocus={(e) => e.preventDefault()}
      >
        <div className="px-3 py-2 border-b">
          <p className="text-xs font-medium">
            {t('admin.attachments.pickerTitle')}
          </p>
          <p className="text-[10px] text-muted-foreground">
            {t('admin.attachments.pickerHint')}
          </p>
        </div>
        <div className="max-h-64 overflow-y-auto">
          {attachments.length === 0 ? (
            <div className="px-3 py-6 text-center text-xs text-muted-foreground">
              {t('admin.attachments.empty')}
            </div>
          ) : (
            <div className="py-1">
              {attachments.map((att) => {
                const isImg = isImageType(att.content_type);
                return (
                  <div
                    key={att.id}
                    draggable
                    onDragStart={(e) => handleDragStart(e, att)}
                    onClick={() => handleClick(att)}
                    className="flex items-center gap-2 px-2 py-1.5 mx-1 rounded-md cursor-pointer hover:bg-accent transition-colors group"
                  >
                    <GripVertical className="h-3 w-3 text-muted-foreground/40 shrink-0 group-hover:text-muted-foreground cursor-grab" />
                    {isImg ? (
                      <AuthImage
                        src={attachmentUrl(apiBasePath, problemId, att.id)}
                        alt={att.path}
                        className="h-8 w-8 rounded object-cover shrink-0 border"
                      />
                    ) : (
                      <div className="h-8 w-8 rounded border bg-muted/50 flex items-center justify-center shrink-0">
                        <FileTypeIcon contentType={att.content_type} />
                      </div>
                    )}
                    <div className="flex-1 min-w-0">
                      <div className="text-xs font-medium truncate">
                        {att.path}
                      </div>
                      <div className="text-[10px] text-muted-foreground">
                        {formatBytes(att.size)}
                        {att.content_type &&
                          ` · ${att.content_type.split('/')[1]}`}
                      </div>
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </div>
      </PopoverContent>
    </Popover>
  );
}
