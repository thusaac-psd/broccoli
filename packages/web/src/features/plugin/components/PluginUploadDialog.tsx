import { useApiClient, useErrorMessage } from '@broccoli/web-sdk/api';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import type { PluginArchiveSummary } from '@broccoli/web-sdk/plugin';
import {
  Badge,
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@broccoli/web-sdk/ui';
import { Loader2, ShieldAlert, Upload } from 'lucide-react';
import { useRef, useState } from 'react';
import { toast } from 'sonner';

/** The upload endpoints read the archive from this multipart field. */
function archiveForm(file: File, replaces?: string): FormData {
  const form = new FormData();
  if (replaces) form.append('replaces', replaces);
  form.append('plugin', file);
  return form;
}

/**
 * What the archive replaces, as the inspection reported it: the server
 * refuses the upload if that changed since, instead of replacing a version
 * the admin never saw.
 */
function replacesOf(summary: PluginArchiveSummary): string {
  const current = summary.current;
  if (!current) return 'new';
  return current.source === 'upload' && current.archive_hash
    ? current.archive_hash
    : 'bundle';
}

function formatBytes(bytes: number): string {
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
}

/**
 * Pick a plugin archive, show what it would publish (checked by the server
 * without publishing), then publish it to every server on confirmation.
 */
export function PluginUploadDialog({
  open,
  onOpenChange,
  onPublished,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onPublished: (pluginId: string) => void;
}) {
  const { t } = useTranslation();
  const errorMessage = useErrorMessage();
  const apiClient = useApiClient();
  const fileInputRef = useRef<HTMLInputElement>(null);
  const [file, setFile] = useState<File | null>(null);
  const [summary, setSummary] = useState<PluginArchiveSummary | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);
  const [publishing, setPublishing] = useState(false);
  const busy = checking || publishing;

  const reset = () => {
    setFile(null);
    setSummary(null);
    setError(null);
    if (fileInputRef.current) fileInputRef.current.value = '';
  };

  const handleOpenChange = (next: boolean) => {
    if (busy) return;
    if (!next) reset();
    onOpenChange(next);
  };

  const inspect = async (selected: File) => {
    setFile(selected);
    setSummary(null);
    setError(null);
    setChecking(true);
    try {
      const { data, error } = await apiClient.POST(
        '/admin/plugins/upload/inspect',
        {
          body: { plugin: selected },
          bodySerializer: () => archiveForm(selected),
        },
      );
      if (error || !data) {
        setError(errorMessage(error, t('plugins.upload.inspectError')));
      } else {
        setSummary(data);
      }
    } finally {
      setChecking(false);
    }
  };

  const publish = async () => {
    if (!file || !summary) return;
    setPublishing(true);
    try {
      const replaces = replacesOf(summary);
      const { data, error } = await apiClient.POST('/admin/plugins/upload', {
        body: { plugin: file, replaces },
        bodySerializer: () => archiveForm(file, replaces),
      });
      if (error || !data) {
        setError(errorMessage(error, t('plugins.upload.error')));
        return;
      }
      toast.success(
        t('plugins.upload.success', {
          id: data.plugin_id,
          version: data.version,
        }),
      );
      onPublished(data.plugin_id);
      reset();
      onOpenChange(false);
    } finally {
      setPublishing(false);
    }
  };

  const current = summary?.current;

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>{t('plugins.upload.title')}</DialogTitle>
          <DialogDescription>
            {t('plugins.upload.description')}
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-4">
          <input
            ref={fileInputRef}
            type="file"
            accept=".tar.gz,.tgz,application/gzip,application/x-gzip"
            className="hidden"
            data-testid="plugin-archive-input"
            onChange={(e) => {
              const selected = e.target.files?.[0];
              if (selected) void inspect(selected);
            }}
          />
          <Button
            type="button"
            variant="outline"
            className="w-full"
            disabled={busy}
            onClick={() => fileInputRef.current?.click()}
          >
            {checking ? (
              <Loader2 className="h-4 w-4 mr-2 animate-spin" />
            ) : (
              <Upload className="h-4 w-4 mr-2" />
            )}
            {checking
              ? t('plugins.upload.checking')
              : (file?.name ?? t('plugins.upload.selectFile'))}
          </Button>

          {error && (
            <p className="text-sm text-destructive break-words" role="alert">
              {error}
            </p>
          )}

          {summary && (
            <div
              className="rounded-md border p-3 space-y-3 text-sm"
              data-testid="plugin-archive-summary"
            >
              <div>
                <p className="font-medium">{summary.name}</p>
                <p className="font-mono text-xs text-muted-foreground">
                  {t('plugins.upload.summary', {
                    id: summary.id,
                    version: summary.version,
                    size: formatBytes(summary.size_bytes),
                  })}
                </p>
                {summary.description && (
                  <p className="text-muted-foreground mt-1">
                    {summary.description}
                  </p>
                )}
              </div>
              <Badge variant={current ? 'secondary' : 'outline'}>
                {!current
                  ? t('plugins.upload.new')
                  : t(
                      current.source === 'upload'
                        ? 'plugins.upload.replacesUpload'
                        : 'plugins.upload.replacesBundle',
                      { version: current.version ?? '?' },
                    )}
              </Badge>
              <div className="space-y-1">
                <p className="text-xs font-medium text-muted-foreground flex items-center gap-1">
                  <ShieldAlert className="h-3.5 w-3.5" />
                  {t('plugins.upload.permissions')}
                </p>
                {summary.permissions.length > 0 ? (
                  <div className="flex flex-wrap gap-1">
                    {summary.permissions.map((p) => (
                      <Badge key={p} variant="secondary" className="text-xs">
                        {p}
                      </Badge>
                    ))}
                  </div>
                ) : (
                  <p className="text-xs text-muted-foreground">
                    {t('plugins.upload.noPermissions')}
                  </p>
                )}
              </div>
              {summary.sql_read.length > 0 && (
                <div className="space-y-1">
                  <p className="text-xs font-medium text-muted-foreground flex items-center gap-1">
                    <ShieldAlert className="h-3.5 w-3.5" />
                    {t('plugins.upload.sqlRead')}
                  </p>
                  <div className="flex flex-wrap gap-1">
                    {summary.sql_read.map((scope) => (
                      <Badge
                        key={scope}
                        variant="secondary"
                        className="font-mono text-xs"
                      >
                        {scope}
                      </Badge>
                    ))}
                  </div>
                </div>
              )}
              <p className="font-mono text-[11px] text-muted-foreground break-all">
                {t('plugins.archiveHash', { hash: summary.archive_hash })}
              </p>
              <p className="text-xs text-muted-foreground">
                {t('plugins.upload.rollout')}
              </p>
            </div>
          )}
        </div>

        <DialogFooter>
          <Button
            type="button"
            disabled={!summary || busy}
            onClick={() => void publish()}
          >
            {publishing && <Loader2 className="h-4 w-4 mr-2 animate-spin" />}
            {publishing
              ? t('plugins.upload.publishing')
              : t('plugins.upload.publish')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
