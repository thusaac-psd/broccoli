import { useApiBasePath, useApiClient } from '@broccoli/web-sdk/api';
import { queryState } from '@broccoli/web-sdk/hooks';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import {
  Button,
  FileDropZone,
  Input,
  QueryErrorState,
  useConfirmDialog,
} from '@broccoli/web-sdk/ui';
import { formatBytes } from '@broccoli/web-sdk/utils';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import {
  Check,
  Copy,
  Download,
  File as FileIcon,
  FileArchive,
  FileText,
  Loader2,
  Trash2,
  Upload,
  X,
} from 'lucide-react';
import { useCallback, useMemo, useState } from 'react';
import { toast } from 'sonner';

import { AuthImage } from '@/components/AuthImage';
import {
  type Attachment,
  attachmentMarkdownRef,
  attachmentsQueryKey,
  attachmentUrl,
  fetchAttachments,
  isImageType,
} from '@/features/problem/api/attachments';
import { useAttachmentUpload } from '@/features/problem/hooks/useAttachmentUpload';

const NO_ATTACHMENTS: Attachment[] = [];

interface StagedFile {
  id: string;
  file: File;
  path: string;
}

let nextStagedId = 0;

interface AttachmentsSectionProps {
  problemId: number;
}

function FileTypeIcon({ contentType }: { contentType: string | null }) {
  if (contentType?.startsWith('text/'))
    return <FileText className="h-4 w-4 shrink-0 text-muted-foreground" />;
  if (
    contentType?.includes('zip') ||
    contentType?.includes('archive') ||
    contentType?.includes('compressed')
  )
    return <FileArchive className="h-4 w-4 shrink-0 text-muted-foreground" />;
  return <FileIcon className="h-4 w-4 shrink-0 text-muted-foreground" />;
}

export function AttachmentsSection({ problemId }: AttachmentsSectionProps) {
  const { t } = useTranslation();
  const apiClient = useApiClient();
  const apiBasePath = useApiBasePath();
  const queryClient = useQueryClient();
  const [confirm, confirmDialog] = useConfirmDialog();
  const { upload: uploadFile } = useAttachmentUpload(problemId);

  const [staged, setStaged] = useState<StagedFile[]>([]);
  const [uploading, setUploading] = useState(false);
  const [uploadingName, setUploadingName] = useState('');
  const [uploadIndex, setUploadIndex] = useState(0);
  const [uploadTotal, setUploadTotal] = useState(0);
  const [copiedId, setCopiedId] = useState<string | null>(null);

  const queryKey = useMemo(() => attachmentsQueryKey(problemId), [problemId]);

  const filesQuery = useQuery({
    queryKey,
    queryFn: () => fetchAttachments(apiClient, problemId),
  });
  const filesState = queryState(filesQuery);
  const files = filesQuery.data ?? NO_ATTACHMENTS;

  const handleFilesSelected = useCallback((fileList: File[]) => {
    const newStaged = fileList.map((file) => ({
      id: `staged-${++nextStagedId}`,
      file,
      path: file.name,
    }));
    setStaged((prev) => [...prev, ...newStaged]);
  }, []);

  const updateStagedPath = useCallback((id: string, value: string) => {
    setStaged((prev) =>
      prev.map((s) => (s.id === id ? { ...s, path: value } : s)),
    );
  }, []);

  const removeStagedFile = useCallback((id: string) => {
    setStaged((prev) => prev.filter((s) => s.id !== id));
  }, []);

  const clearStaged = useCallback(() => setStaged([]), []);

  const handleUploadSingle = useCallback(
    async (item: StagedFile) => {
      setUploading(true);
      setUploadingName(item.file.name);

      const attachment = await uploadFile(item.file, item.path);

      setUploading(false);
      setUploadingName('');

      if (attachment) {
        setStaged((prev) => prev.filter((s) => s.id !== item.id));
        const url = attachmentUrl(apiBasePath, problemId, attachment.id);
        const md = attachmentMarkdownRef(
          attachment.path,
          url,
          isImageType(attachment.content_type),
        );
        toast.success(t('admin.attachments.uploaded'), {
          action: {
            label: t('admin.attachments.copyMarkdown'),
            onClick: () => navigator.clipboard.writeText(md),
          },
        });
      }
    },
    [apiBasePath, uploadFile, problemId, t],
  );

  const handleUploadAll = useCallback(async () => {
    setUploading(true);
    const remaining: StagedFile[] = [];
    let successCount = 0;
    const total = staged.length;
    setUploadTotal(total);

    for (let i = 0; i < staged.length; i++) {
      const item = staged[i];
      setUploadIndex(i + 1);
      setUploadingName(item.file.name);

      const attachment = await uploadFile(item.file, item.path);
      if (attachment) {
        successCount++;
      } else {
        remaining.push(item);
      }
    }

    setUploading(false);
    setUploadingName('');
    setUploadIndex(0);
    setUploadTotal(0);
    setStaged(remaining);

    if (successCount > 0) {
      toast.success(t('admin.attachments.uploaded') + ` (${successCount})`);
    }
  }, [staged, uploadFile, t]);

  const handleCopyMarkdown = useCallback(
    (file: Attachment) => {
      const url = attachmentUrl(apiBasePath, problemId, file.id);
      const md = attachmentMarkdownRef(
        file.path,
        url,
        isImageType(file.content_type),
      );
      navigator.clipboard.writeText(md);
      setCopiedId(file.id);
      toast.success(t('admin.attachments.markdownCopied'));
      setTimeout(() => setCopiedId(null), 2000);
    },
    [apiBasePath, problemId, t],
  );

  const handleDelete = useCallback(
    (file: Attachment) =>
      confirm({
        title: t('admin.deleteConfirm'),
        description: file.filename,
        confirmLabel: t('admin.delete'),
        destructive: true,
        failureMessage: t('admin.attachments.deleteError'),
        onConfirm: async () => {
          const { error } = await apiClient.DELETE(
            '/problems/{id}/attachments/{ref_id}',
            { params: { path: { id: problemId, ref_id: file.id } } },
          );
          if (error) throw error;
          toast.success(t('admin.attachments.deleted'));
          queryClient.invalidateQueries({ queryKey });
        },
      }),
    [apiClient, confirm, problemId, queryClient, queryKey, t],
  );

  const handleDownload = useCallback(
    async (file: Attachment) => {
      try {
        const { data, error } = await apiClient.GET(
          '/problems/{id}/attachments/{ref_id}',
          {
            params: { path: { id: problemId, ref_id: file.id } },
            parseAs: 'blob',
          },
        );
        if (error) throw error;

        const url = URL.createObjectURL(data as Blob);
        const a = document.createElement('a');
        a.href = url;
        a.download = file.filename;
        a.style.display = 'none';
        document.body.appendChild(a);
        a.click();
        document.body.removeChild(a);
        setTimeout(() => URL.revokeObjectURL(url), 1000);
      } catch {
        toast.error(t('admin.attachments.downloadError'));
      }
    },
    [apiClient, problemId, t],
  );

  const uploadProgressText = useMemo(() => {
    if (!uploading) return '';
    if (uploadTotal > 1) {
      return t('admin.attachments.uploadingBatch', {
        current: String(uploadIndex),
        total: String(uploadTotal),
        filename: uploadingName,
      });
    }
    return t('admin.attachments.uploading', { filename: uploadingName });
  }, [uploading, uploadTotal, uploadIndex, uploadingName, t]);

  return (
    <div className="space-y-4">
      <p className="text-sm text-muted-foreground">
        {t('admin.attachments.description')}
      </p>

      {/* Drop zone */}
      {uploading ? (
        <div className="flex items-center gap-2 rounded-md border-2 border-dashed px-4 py-6 text-sm text-muted-foreground">
          <Loader2 className="h-4 w-4 animate-spin shrink-0" />
          <span>{uploadProgressText}</span>
        </div>
      ) : (
        <FileDropZone onFilesSelected={handleFilesSelected} multiple>
          <span className="text-sm">{t('admin.attachments.dropzone')}</span>
        </FileDropZone>
      )}

      {/* Staged files */}
      {staged.length > 0 && (
        <div className="space-y-2">
          <div className="flex items-center justify-between">
            <span className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
              {t('admin.attachments.staged', {
                count: String(staged.length),
              })}
            </span>
            <div className="flex gap-1.5">
              <Button
                type="button"
                variant="ghost"
                size="sm"
                onClick={clearStaged}
                disabled={uploading}
              >
                {t('admin.attachments.clear')}
              </Button>
              <Button
                type="button"
                size="sm"
                onClick={handleUploadAll}
                disabled={uploading}
              >
                <Upload className="h-3.5 w-3.5 mr-1" />
                {t('admin.attachments.uploadAll')}
              </Button>
            </div>
          </div>

          <div className="rounded-md border divide-y">
            {staged.map((item) => (
              <div key={item.id} className="flex items-center gap-2 px-3 py-2">
                {/* Filename */}
                <span
                  className="text-xs font-medium truncate w-28 shrink-0"
                  title={item.file.name}
                >
                  {item.file.name}
                </span>

                {/* Virtual path */}
                <Input
                  value={item.path}
                  onChange={(e) => updateStagedPath(item.id, e.target.value)}
                  placeholder={item.file.name}
                  disabled={uploading}
                  className="h-7 text-xs flex-1 min-w-0"
                />

                {/* Size */}
                <span className="text-[10px] text-muted-foreground shrink-0 w-14 text-right">
                  {formatBytes(item.file.size)}
                </span>

                {/* Upload single */}
                <Button
                  type="button"
                  variant="ghost"
                  size="icon"
                  className="h-7 w-7 shrink-0 text-primary hover:text-primary"
                  onClick={() => handleUploadSingle(item)}
                  disabled={uploading}
                  title={t('admin.attachments.upload')}
                >
                  <Upload className="h-3.5 w-3.5" />
                </Button>

                {/* Remove from staging */}
                <Button
                  type="button"
                  variant="ghost"
                  size="icon"
                  className="h-7 w-7 shrink-0 text-muted-foreground hover:text-destructive"
                  onClick={() => removeStagedFile(item.id)}
                  disabled={uploading}
                >
                  <X className="h-3.5 w-3.5" />
                </Button>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Existing file listing */}
      {filesState.status === 'error' ? (
        <QueryErrorState
          className="py-4"
          error={filesState.error}
          retry={filesState.retry}
          retrying={filesState.retrying}
          failure={t('admin.attachments.loadFailed')}
        />
      ) : filesState.status === 'loading' ? (
        <div className="py-4 text-center text-sm text-muted-foreground">
          {t('admin.loading')}
        </div>
      ) : files.length === 0 && staged.length === 0 ? (
        <div className="py-4 text-center text-sm text-muted-foreground">
          {t('admin.attachments.empty')}
        </div>
      ) : files.length > 0 ? (
        <div className="rounded-md border divide-y">
          {files.map((file) => (
            <div
              key={file.id}
              className="flex items-center gap-2 px-3 py-2 text-sm"
            >
              {/* Thumbnail or file type icon */}
              {isImageType(file.content_type) ? (
                <AuthImage
                  src={attachmentUrl(apiBasePath, problemId, file.id)}
                  alt={file.path}
                  className="h-8 w-8 rounded object-cover shrink-0 border"
                />
              ) : (
                <FileTypeIcon contentType={file.content_type} />
              )}

              <span className="flex-1 truncate font-mono text-xs">
                {file.path}
              </span>
              <span className="text-xs text-muted-foreground shrink-0">
                {formatBytes(file.size)}
              </span>

              {/* Copy Markdown */}
              <Button
                type="button"
                variant="ghost"
                size="icon"
                className="h-7 w-7 shrink-0"
                onClick={() => handleCopyMarkdown(file)}
                title={t('admin.attachments.copyMarkdown')}
              >
                {copiedId === file.id ? (
                  <Check className="h-3.5 w-3.5 text-green-500" />
                ) : (
                  <Copy className="h-3.5 w-3.5" />
                )}
              </Button>

              <Button
                type="button"
                variant="ghost"
                size="icon"
                className="h-7 w-7 shrink-0"
                onClick={() => handleDownload(file)}
              >
                <Download className="h-3.5 w-3.5" />
              </Button>
              <Button
                type="button"
                variant="ghost"
                size="icon"
                className="h-7 w-7 shrink-0 text-destructive hover:text-destructive"
                onClick={() => handleDelete(file)}
              >
                <Trash2 className="h-3.5 w-3.5" />
              </Button>
            </div>
          ))}
        </div>
      ) : null}
      {confirmDialog}
    </div>
  );
}
