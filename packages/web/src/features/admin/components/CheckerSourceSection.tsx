import { useApiClient, useErrorMessage } from '@broccoli/web-sdk/api';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Button, FileDropZone, useConfirmDialog } from '@broccoli/web-sdk/ui';
import { formatBytes } from '@broccoli/web-sdk/utils';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { FileCode, Loader2, Plus, Trash2, Upload } from 'lucide-react';
import { useCallback, useMemo, useState } from 'react';
import { toast } from 'sonner';

interface CheckerSourceFile {
  filename: string;
  content: string;
}

interface CheckerSourceResponse {
  files: CheckerSourceFile[] | null;
}

interface StagedFile {
  id: string;
  filename: string;
  content: string;
  size: number;
}

let nextStagedId = 0;

function checkerSourceQueryKey(problemId: number) {
  return ['checker-source', problemId];
}

interface CheckerSourceSectionProps {
  problemId: number;
}

export function CheckerSourceSection({ problemId }: CheckerSourceSectionProps) {
  const { t } = useTranslation();
  const errorMessage = useErrorMessage();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();
  const [confirm, confirmDialog] = useConfirmDialog();

  const [staged, setStaged] = useState<StagedFile[]>([]);
  const [uploading, setUploading] = useState(false);

  const queryKey = checkerSourceQueryKey(problemId);

  const { data: existing, isLoading } = useQuery({
    queryKey,
    queryFn: async (): Promise<CheckerSourceFile[]> => {
      const { data, error } = await apiClient.GET(
        '/problems/{id}/checker-source',
        { params: { path: { id: problemId } } },
      );
      if (error) throw error;
      return (data as CheckerSourceResponse)?.files ?? [];
    },
  });

  const files = useMemo(() => existing ?? [], [existing]);

  const handleFilesSelected = useCallback((fileList: File[]) => {
    for (const file of fileList) {
      const reader = new FileReader();
      reader.onload = () => {
        const content = reader.result as string;
        setStaged((prev) => [
          ...prev,
          {
            id: `staged-${++nextStagedId}`,
            filename: file.name,
            content,
            size: file.size,
          },
        ]);
      };
      reader.readAsText(file);
    }
  }, []);

  const removeStagedFile = useCallback((id: string) => {
    setStaged((prev) => prev.filter((s) => s.id !== id));
  }, []);

  const handleUpload = useCallback(async () => {
    if (staged.length === 0) return;
    setUploading(true);

    try {
      const merged = new Map<string, CheckerSourceFile>();
      for (const f of files) {
        merged.set(f.filename, f);
      }
      for (const s of staged) {
        merged.set(s.filename, { filename: s.filename, content: s.content });
      }

      const payload = { files: Array.from(merged.values()) };

      const { error } = await apiClient.PUT('/problems/{id}/checker-source', {
        params: { path: { id: problemId } },
        body: payload as never,
      });

      if (error) {
        throw error;
      }

      setStaged([]);
      toast.success(t('admin.checkerSource.uploaded'));
      queryClient.invalidateQueries({ queryKey });
    } catch (err) {
      toast.error(errorMessage(err, t('admin.checkerSource.uploadError')));
    } finally {
      setUploading(false);
    }
  }, [
    errorMessage,
    staged,
    files,
    apiClient,
    problemId,
    queryClient,
    queryKey,
    t,
  ]);

  const handleDeleteFile = useCallback(
    async (filename: string) => {
      const remaining = files.filter((f) => f.filename !== filename);

      try {
        if (remaining.length === 0) {
          const { error } = await apiClient.DELETE(
            '/problems/{id}/checker-source',
            { params: { path: { id: problemId } } },
          );
          if (error) throw error;
        } else {
          const { error } = await apiClient.PUT(
            '/problems/{id}/checker-source',
            {
              params: { path: { id: problemId } },
              body: { files: remaining } as never,
            },
          );
          if (error) throw error;
        }

        toast.success(t('admin.checkerSource.deleted'));
        queryClient.invalidateQueries({ queryKey });
      } catch (err) {
        toast.error(errorMessage(err, t('admin.checkerSource.deleteError')));
      }
    },
    [errorMessage, files, apiClient, problemId, queryClient, queryKey, t],
  );

  const handleClearAll = useCallback(
    () =>
      confirm({
        title: t('admin.deleteConfirm'),
        confirmLabel: t('admin.delete'),
        destructive: true,
        failureMessage: t('admin.checkerSource.deleteError'),
        onConfirm: async () => {
          const { error } = await apiClient.DELETE(
            '/problems/{id}/checker-source',
            { params: { path: { id: problemId } } },
          );
          if (error) throw error;
          toast.success(t('admin.checkerSource.cleared'));
          queryClient.invalidateQueries({ queryKey });
        },
      }),
    [apiClient, confirm, problemId, queryClient, queryKey, t],
  );

  return (
    <div className="space-y-4">
      <p className="text-sm text-muted-foreground">
        {t('admin.checkerSource.description')}
      </p>

      {/* Drop zone */}
      {uploading ? (
        <div className="flex items-center gap-2 rounded-md border-2 border-dashed px-4 py-6 text-sm text-muted-foreground">
          <Loader2 className="h-4 w-4 animate-spin shrink-0" />
          <span>{t('admin.checkerSource.uploading')}</span>
        </div>
      ) : (
        <FileDropZone
          onFilesSelected={handleFilesSelected}
          multiple
          accept=".cpp,.cc,.cxx,.c,.h,.hpp"
        >
          <span className="text-sm">{t('admin.checkerSource.dropzone')}</span>
        </FileDropZone>
      )}

      {/* Staged files */}
      {staged.length > 0 && (
        <div className="space-y-2">
          <div className="flex items-center justify-between">
            <span className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
              {t('admin.checkerSource.staged', {
                count: String(staged.length),
              })}
            </span>
            <div className="flex gap-1.5">
              <Button
                type="button"
                variant="ghost"
                size="sm"
                onClick={() => setStaged([])}
                disabled={uploading}
              >
                {t('admin.checkerSource.clear')}
              </Button>
              <Button
                type="button"
                size="sm"
                onClick={handleUpload}
                disabled={uploading}
              >
                <Upload className="h-3.5 w-3.5 mr-1" />
                {t('admin.checkerSource.upload')}
              </Button>
            </div>
          </div>

          <div className="rounded-md border divide-y">
            {staged.map((item) => (
              <div key={item.id} className="flex items-center gap-2 px-3 py-2">
                <FileCode className="h-4 w-4 shrink-0 text-muted-foreground" />
                <span
                  className="text-xs font-mono truncate flex-1"
                  title={item.filename}
                >
                  {item.filename}
                </span>
                <span className="text-[10px] text-muted-foreground shrink-0">
                  {formatBytes(item.size)}
                </span>
                <Button
                  type="button"
                  variant="ghost"
                  size="icon"
                  className="h-7 w-7 shrink-0 text-muted-foreground hover:text-destructive"
                  onClick={() => removeStagedFile(item.id)}
                  disabled={uploading}
                >
                  <Plus className="h-3.5 w-3.5 rotate-45" />
                </Button>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Existing files */}
      {isLoading ? (
        <div className="py-4 text-center text-sm text-muted-foreground">
          {t('admin.loading')}
        </div>
      ) : files.length === 0 && staged.length === 0 ? (
        <div className="py-4 text-center text-sm text-muted-foreground">
          {t('admin.checkerSource.empty')}
        </div>
      ) : files.length > 0 ? (
        <div className="space-y-2">
          <div className="flex items-center justify-between">
            <span className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
              {t('admin.checkerSource.fileCount', {
                count: String(files.length),
              })}
            </span>
            <Button
              type="button"
              variant="ghost"
              size="sm"
              className="text-destructive hover:text-destructive"
              onClick={handleClearAll}
            >
              <Trash2 className="h-3.5 w-3.5 mr-1" />
              {t('admin.checkerSource.clearAll')}
            </Button>
          </div>
          <div className="rounded-md border divide-y">
            {files.map((file) => (
              <div
                key={file.filename}
                className="flex items-center gap-2 px-3 py-2 text-sm"
              >
                <FileCode className="h-4 w-4 shrink-0 text-muted-foreground" />
                <span className="flex-1 truncate font-mono text-xs">
                  {file.filename}
                </span>
                <span className="text-xs text-muted-foreground shrink-0">
                  {formatBytes(new Blob([file.content]).size)}
                </span>
                <Button
                  type="button"
                  variant="ghost"
                  size="icon"
                  className="h-7 w-7 shrink-0 text-destructive hover:text-destructive"
                  onClick={() => handleDeleteFile(file.filename)}
                >
                  <Trash2 className="h-3.5 w-3.5" />
                </Button>
              </div>
            ))}
          </div>
        </div>
      ) : null}
      {confirmDialog}
    </div>
  );
}
