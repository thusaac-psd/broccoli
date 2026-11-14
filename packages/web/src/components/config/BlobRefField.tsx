import { useApiClient, useErrorMessage } from '@broccoli/web-sdk/api';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Button, FileDropZone, Label } from '@broccoli/web-sdk/ui';
import { File, Loader2, Upload, X } from 'lucide-react';
import { useCallback, useState } from 'react';

interface BlobRefValue {
  filename: string;
  hash?: string;
  content_hash?: string;
}

export function BlobRefField({
  label,
  description,
  value,
  onChange,
  isExplicit,
}: Readonly<{
  label: string;
  description?: string;
  value: BlobRefValue | undefined;
  onChange: (v: BlobRefValue | undefined) => void;
  isExplicit: boolean;
}>) {
  const { t } = useTranslation();
  const errorMessage = useErrorMessage();
  const apiClient = useApiClient();
  const [uploading, setUploading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [uploadingName, setUploadingName] = useState<string>('');

  const handleUpload = useCallback(
    async (file: globalThis.File) => {
      setError(null);
      setUploading(true);
      setUploadingName(file.name);

      try {
        const formData = new FormData();
        formData.append('file', file);

        const { data, error: apiError } = await apiClient.POST(
          '/config/upload',
          {
            body: formData,
            bodySerializer: (body) => body as BodyInit,
          },
        );

        if (apiError) throw apiError;

        onChange({
          filename: data.filename,
          hash: data.content_hash,
        });
      } catch (err) {
        setError(errorMessage(err, t('config.blob.uploadFailed')));
      } finally {
        setUploading(false);
        setUploadingName('');
      }
    },
    [apiClient, errorMessage, onChange, t],
  );

  const handleFilesSelected = useCallback(
    (files: globalThis.File[]) => {
      if (files[0]) handleUpload(files[0]);
    },
    [handleUpload],
  );

  return (
    <div className="flex flex-col gap-1.5">
      <div className="space-y-1">
        <div className="flex items-center gap-2">
          <Label className="text-xs font-medium text-muted-foreground uppercase tracking-wider">
            {label}
          </Label>
          {!isExplicit && (
            <span className="inline-flex items-center rounded-full border border-dashed px-2 py-0.5 text-[10px] font-medium uppercase tracking-wide text-muted-foreground">
              {t('config.blob.default')}
            </span>
          )}
        </div>
        {description && (
          <p className="text-xs text-muted-foreground">{description}</p>
        )}
      </div>

      {uploading ? (
        <div className="flex items-center gap-2 rounded-md border border-dashed px-3 py-2 text-sm text-muted-foreground">
          <Loader2 className="h-3.5 w-3.5 animate-spin shrink-0" />
          <span className="truncate">{uploadingName}</span>
        </div>
      ) : value?.filename ? (
        <div className="flex items-center gap-2">
          <div className="flex items-center gap-2 min-w-0 flex-1 rounded-md border bg-muted/40 px-3 py-2 text-sm">
            <File className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
            <span className="font-medium truncate">{value.filename}</span>
            {(value.hash ?? value.content_hash) && (
              <span className="text-muted-foreground font-mono text-xs truncate">
                {(value.hash ?? value.content_hash)!.slice(0, 12)}...
              </span>
            )}
          </div>
          <FileDropZone
            onFilesSelected={handleFilesSelected}
            className="border-0 p-0 rounded-none inline-flex"
            aria-label={t('config.blob.replaceLabel', { name: value.filename })}
          >
            <Button
              type="button"
              variant="outline"
              size="sm"
              className="shrink-0 pointer-events-none"
              tabIndex={-1}
            >
              <Upload className="h-3 w-3 mr-1.5" />
              {t('config.blob.replace')}
            </Button>
          </FileDropZone>
          <Button
            type="button"
            variant="ghost"
            size="icon"
            className="shrink-0 h-8 w-8 text-muted-foreground hover:text-destructive"
            onClick={() => {
              setError(null);
              onChange(undefined);
            }}
          >
            <X className="h-3.5 w-3.5" />
          </Button>
        </div>
      ) : (
        <FileDropZone
          onFilesSelected={handleFilesSelected}
          className="py-4"
          aria-label={t('config.blob.uploadLabel', { name: label })}
        >
          <Upload className="h-4 w-4" />
          <span className="text-xs">{t('config.blob.prompt')}</span>
        </FileDropZone>
      )}

      {error && (
        <div className="flex items-center gap-2 text-sm text-destructive">
          <span>{error}</span>
          <FileDropZone
            onFilesSelected={handleFilesSelected}
            className="border-0 p-0 rounded-none inline-flex"
            aria-label={t('config.blob.retryLabel')}
          >
            <Button
              type="button"
              variant="ghost"
              size="sm"
              className="h-auto py-0.5 px-1.5 text-xs pointer-events-none"
              tabIndex={-1}
            >
              {t('common.retry')}
            </Button>
          </FileDropZone>
        </div>
      )}
    </div>
  );
}
