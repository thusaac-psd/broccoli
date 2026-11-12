import { getErrorMessage, useApiClient } from '@broccoli/web-sdk/api';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import {
  Button,
  FileDropZone,
  Input,
  useConfirmDialog,
} from '@broccoli/web-sdk/ui';
import { formatBytes } from '@broccoli/web-sdk/utils';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { Download, Loader2, Trash2, Upload, X } from 'lucide-react';
import { useCallback, useMemo, useRef, useState } from 'react';
import { toast } from 'sonner';

import {
  type AdditionalFile,
  additionalFilesQueryKey,
  fetchAdditionalFiles,
  groupFilesByLanguage,
} from '@/features/problem/api/additional-files';
import { fetchSupportedLanguages } from '@/features/problem/api/fetch-supported-languages';

interface StagedFile {
  id: string;
  file: File;
  language: string;
  path: string;
}

let nextStagedId = 0;

interface AdditionalFilesSectionProps {
  problemId: number;
}

export function AdditionalFilesSection({
  problemId,
}: AdditionalFilesSectionProps) {
  const { t } = useTranslation();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();
  const [confirm, confirmDialog] = useConfirmDialog();

  const [staged, setStaged] = useState<StagedFile[]>([]);
  const [uploading, setUploading] = useState(false);
  const [uploadingName, setUploadingName] = useState('');
  const defaultLanguageRef = useRef('');

  const queryKey = useMemo(
    () => additionalFilesQueryKey(problemId),
    [problemId],
  );

  const { data: files = [], isLoading: filesLoading } = useQuery({
    queryKey,
    queryFn: () => fetchAdditionalFiles(apiClient, problemId),
  });

  const { data: languages = [], isLoading: languagesLoading } = useQuery({
    queryKey: ['supported-languages'],
    queryFn: () => fetchSupportedLanguages(apiClient),
    staleTime: 60_000,
  });

  // --- Staging ---

  const handleFilesSelected = useCallback((fileList: File[]) => {
    const newStaged = fileList.map((file) => ({
      id: `staged-${++nextStagedId}`,
      file,
      language: defaultLanguageRef.current,
      path: file.name,
    }));
    setStaged((prev) => [...prev, ...newStaged]);
  }, []);

  const updateStagedField = useCallback(
    (id: string, field: 'language' | 'path', value: string) => {
      setStaged((prev) =>
        prev.map((s) => (s.id === id ? { ...s, [field]: value } : s)),
      );
    },
    [],
  );

  const removeStagedFile = useCallback((id: string) => {
    setStaged((prev) => prev.filter((s) => s.id !== id));
  }, []);

  const clearStaged = useCallback(() => setStaged([]), []);

  // --- Upload ---

  const uploadOne = useCallback(
    async (item: StagedFile): Promise<boolean> => {
      if (!item.language) {
        toast.error(
          `${item.file.name}: ${t('admin.additionalFiles.selectLanguageFirst')}`,
        );
        return false;
      }

      setUploadingName(item.file.name);

      try {
        const formData = new FormData();
        formData.append('file', item.file);
        formData.append('language', item.language);
        if (item.path.trim() && item.path.trim() !== item.file.name) {
          formData.append('path', item.path.trim());
        }

        const { error } = await apiClient.POST(
          '/problems/{id}/additional-files',
          {
            params: { path: { id: problemId } },
            body: formData,
            bodySerializer: (body) => body as BodyInit,
          },
        );

        if (error) {
          throw new Error(getErrorMessage(error, 'Upload failed'));
        }

        return true;
      } catch (err) {
        toast.error(
          getErrorMessage(err, t('admin.additionalFiles.uploadError')),
        );
        return false;
      }
    },
    [apiClient, problemId, t],
  );

  const handleUploadSingle = useCallback(
    async (item: StagedFile) => {
      setUploading(true);
      const ok = await uploadOne(item);
      setUploading(false);
      setUploadingName('');

      if (ok) {
        setStaged((prev) => prev.filter((s) => s.id !== item.id));
        toast.success(t('admin.additionalFiles.uploaded'));
        queryClient.invalidateQueries({ queryKey });
      }
    },
    [uploadOne, queryClient, queryKey, t],
  );

  const handleUploadAll = useCallback(async () => {
    setUploading(true);
    const remaining: StagedFile[] = [];
    let successCount = 0;

    for (const item of staged) {
      const ok = await uploadOne(item);
      if (ok) {
        successCount++;
      } else {
        remaining.push(item);
      }
    }

    setUploading(false);
    setUploadingName('');
    setStaged(remaining);

    if (successCount > 0) {
      toast.success(t('admin.additionalFiles.uploaded') + ` (${successCount})`);
      queryClient.invalidateQueries({ queryKey });
    }
  }, [staged, uploadOne, queryClient, queryKey, t]);

  // --- Delete / Download ---

  const handleDelete = useCallback(
    (file: AdditionalFile) =>
      confirm({
        title: t('admin.deleteConfirm'),
        description: file.filename,
        confirmLabel: t('admin.delete'),
        destructive: true,
        failureMessage: t('admin.additionalFiles.deleteError'),
        onConfirm: async () => {
          const { error } = await apiClient.DELETE(
            '/problems/{id}/additional-files/{ref_id}',
            { params: { path: { id: problemId, ref_id: file.id } } },
          );
          if (error) throw error;
          toast.success(t('admin.additionalFiles.deleted'));
          queryClient.invalidateQueries({ queryKey });
        },
      }),
    [apiClient, confirm, problemId, queryClient, queryKey, t],
  );

  const handleDownload = useCallback(
    async (file: AdditionalFile) => {
      try {
        const { data, error } = await apiClient.GET(
          '/problems/{id}/additional-files/{ref_id}',
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
        toast.error(t('admin.additionalFiles.downloadError'));
      }
    },
    [apiClient, problemId, t],
  );

  const grouped = groupFilesByLanguage(files);
  const languageKeys = Object.keys(grouped).sort();
  const noLanguages = !languagesLoading && languages.length === 0;

  return (
    <div className="space-y-4">
      <p className="text-sm text-muted-foreground">
        {t('admin.additionalFiles.description')}
      </p>

      {/* Drop zone — always available */}
      {noLanguages ? (
        <p className="text-sm text-muted-foreground">
          {t('admin.additionalFiles.noLanguages')}
        </p>
      ) : uploading ? (
        <div className="flex items-center gap-2 rounded-md border-2 border-dashed px-4 py-6 text-sm text-muted-foreground">
          <Loader2 className="h-4 w-4 animate-spin shrink-0" />
          <span>
            {t('admin.additionalFiles.uploading', {
              filename: uploadingName,
            })}
          </span>
        </div>
      ) : (
        <FileDropZone onFilesSelected={handleFilesSelected} multiple>
          <span className="text-sm">{t('admin.additionalFiles.dropzone')}</span>
        </FileDropZone>
      )}

      {/* Staged files — review & edit before upload */}
      {staged.length > 0 && (
        <div className="space-y-2">
          <div className="flex items-center justify-between">
            <span className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
              {t('admin.additionalFiles.staged', {
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
                {t('admin.additionalFiles.clear')}
              </Button>
              <Button
                type="button"
                size="sm"
                onClick={handleUploadAll}
                disabled={uploading}
              >
                <Upload className="h-3.5 w-3.5 mr-1" />
                {t('admin.additionalFiles.uploadAll')}
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

                {/* Language */}
                <select
                  value={item.language}
                  onChange={(e) => {
                    updateStagedField(item.id, 'language', e.target.value);
                    defaultLanguageRef.current = e.target.value;
                  }}
                  disabled={uploading}
                  className="h-7 w-28 shrink-0 rounded border border-input bg-transparent px-1.5 text-xs focus-visible:outline-hidden focus-visible:ring-1 focus-visible:ring-ring"
                >
                  <option value="">
                    {t('admin.additionalFiles.language')}
                  </option>
                  {languages.map((lang) => (
                    <option key={lang.id} value={lang.id}>
                      {lang.name}
                    </option>
                  ))}
                </select>

                {/* Virtual path */}
                <Input
                  value={item.path}
                  onChange={(e) =>
                    updateStagedField(item.id, 'path', e.target.value)
                  }
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
                  title={t('admin.additionalFiles.upload')}
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
      {filesLoading ? (
        <div className="py-4 text-center text-sm text-muted-foreground">
          {t('admin.loading')}
        </div>
      ) : files.length === 0 && staged.length === 0 ? (
        <div className="py-4 text-center text-sm text-muted-foreground">
          {t('admin.additionalFiles.empty')}
        </div>
      ) : (
        <div className="space-y-3">
          {languageKeys.map((lang) => {
            const langFiles = grouped[lang];
            return (
              <div key={lang}>
                <div className="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1.5">
                  {lang} (
                  {t('admin.additionalFiles.fileCount', {
                    count: String(langFiles.length),
                  })}
                  )
                </div>
                <div className="rounded-md border divide-y">
                  {langFiles.map((file) => (
                    <div
                      key={file.id}
                      className="flex items-center gap-2 px-3 py-2 text-sm"
                    >
                      <span className="flex-1 truncate font-mono text-xs">
                        {file.path}
                      </span>
                      <span className="text-xs text-muted-foreground shrink-0">
                        {formatBytes(file.size)}
                      </span>
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
              </div>
            );
          })}
        </div>
      )}
      {confirmDialog}
    </div>
  );
}
