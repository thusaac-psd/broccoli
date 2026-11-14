import { useApiClient, useErrorMessage } from '@broccoli/web-sdk/api';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import {
  TEST_CASE_MERGE_STRATEGIES,
  type TestCaseMergeStrategy,
} from '@broccoli/web-sdk/problem';
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Input,
  Label,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@broccoli/web-sdk/ui';
import { useQueryClient } from '@tanstack/react-query';
import { Upload } from 'lucide-react';
import { useRef, useState } from 'react';
import { toast } from 'sonner';

import { invalidateProblemTestCases } from '@/features/admin/hooks/test-case-queries';

const DEFAULT_INPUT_FORMAT = '*.in';
const DEFAULT_OUTPUT_FORMAT = '*.out';

interface TestCaseBulkUploadDialogProps {
  problemId: number;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

export function TestCaseBulkUploadDialog({
  problemId,
  open,
  onOpenChange,
}: TestCaseBulkUploadDialogProps) {
  const apiClient = useApiClient();
  const { t } = useTranslation();
  const errorMessage = useErrorMessage();
  const queryClient = useQueryClient();
  const fileInputRef = useRef<HTMLInputElement>(null);

  const [selectedFile, setSelectedFile] = useState<File | null>(null);
  const [inputFormat, setInputFormat] = useState(DEFAULT_INPUT_FORMAT);
  const [outputFormat, setOutputFormat] = useState(DEFAULT_OUTPUT_FORMAT);
  const [strategy, setStrategy] = useState<TestCaseMergeStrategy>('abort');
  const [loading, setLoading] = useState(false);

  const handleFileChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (file) {
      if (
        file.name.endsWith('.zip') ||
        file.type === 'application/zip' ||
        file.type === 'application/x-zip-compressed'
      ) {
        setSelectedFile(file);
      } else {
        toast.error(t('admin.testCases.bulkUpload.invalidFileType'));
      }
    }
  };

  const handleUploadClick = () => {
    fileInputRef.current?.click();
  };

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();

    if (!selectedFile) {
      toast.error(t('admin.testCases.bulkUpload.noFile'));
      return;
    }

    // Validate format patterns
    const validateFormat = (format: string) => {
      if (!format) return true; // Empty is valid (optional)
      const asteriskCount = (format.match(/\*/g) || []).length;
      return asteriskCount === 1;
    };

    if (!inputFormat.trim()) {
      toast.error(t('admin.testCases.bulkUpload.inputFormatRequired'));
      return;
    }

    if (!validateFormat(inputFormat)) {
      toast.error(t('admin.testCases.bulkUpload.invalidInputFormat'));
      return;
    }

    if (!outputFormat.trim()) {
      toast.error(t('admin.testCases.bulkUpload.outputFormatRequired'));
      return;
    }

    if (!validateFormat(outputFormat)) {
      toast.error(t('admin.testCases.bulkUpload.invalidOutputFormat'));
      return;
    }

    setLoading(true);

    try {
      const result = await apiClient.POST('/problems/{id}/test-cases/upload', {
        params: { path: { id: problemId } },
        // File is typed as string in the OpenAPI schema (binary format),
        // but we pass a File object and handle serialization in bodySerializer.
        body: {
          file: selectedFile,
          input_format: inputFormat,
          output_format: outputFormat,
          strategy: strategy as 'abort' | 'skip' | 'overwrite' | 'replace',
        },
        bodySerializer: (body) => {
          const formData = new FormData();
          formData.append('file', selectedFile);
          formData.append('input_format', body.input_format);
          formData.append('output_format', body.output_format);
          formData.append('strategy', body.strategy);
          return formData;
        },
      });

      if (result.error) throw result.error;

      toast.success(t('admin.testCases.bulkUpload.uploadSuccess'));
      invalidateProblemTestCases(queryClient, problemId);
      onOpenChange(false);
      setSelectedFile(null);
      setInputFormat(DEFAULT_INPUT_FORMAT);
      setOutputFormat(DEFAULT_OUTPUT_FORMAT);
      setStrategy('abort');
    } catch (error) {
      console.error('Bulk upload error:', error);
      toast.error(
        errorMessage(error, t('admin.testCases.bulkUpload.uploadError')),
      );
    } finally {
      setLoading(false);
    }
  };

  const handleOpenChange = (newOpen: boolean) => {
    if (!loading) {
      onOpenChange(newOpen);
      if (!newOpen) {
        setSelectedFile(null);
        setInputFormat(DEFAULT_INPUT_FORMAT);
        setOutputFormat(DEFAULT_OUTPUT_FORMAT);
        setStrategy('abort');
      }
    }
  };

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>{t('admin.testCases.bulkUpload.title')}</DialogTitle>
          <DialogDescription>
            {t('admin.testCases.bulkUpload.description')}
          </DialogDescription>
        </DialogHeader>

        <form onSubmit={handleSubmit} className="space-y-4">
          <div className="space-y-2">
            <Label>{t('admin.testCases.bulkUpload.zipFile')}</Label>
            <input
              ref={fileInputRef}
              type="file"
              accept=".zip,application/zip,application/x-zip-compressed"
              onChange={handleFileChange}
              className="hidden"
            />
            <div className="flex gap-2">
              <Button
                type="button"
                variant="outline"
                onClick={handleUploadClick}
                className="flex-1"
              >
                <Upload className="h-4 w-4 mr-2" />
                {selectedFile
                  ? selectedFile.name
                  : t('admin.testCases.bulkUpload.selectFile')}
              </Button>
            </div>
            {selectedFile && (
              <p className="text-xs text-muted-foreground">
                {t('admin.testCases.bulkUpload.fileSize', {
                  size: (selectedFile.size / 1024 / 1024).toFixed(2),
                })}
              </p>
            )}
          </div>

          <div className="space-y-2">
            <Label htmlFor="input-format">
              {t('admin.testCases.bulkUpload.inputFormat')}
            </Label>
            <Input
              id="input-format"
              value={inputFormat}
              onChange={(e) => setInputFormat(e.target.value)}
              placeholder={DEFAULT_INPUT_FORMAT}
              required
            />
            <p className="text-xs text-muted-foreground">
              {t('admin.testCases.bulkUpload.formatHint')}
            </p>
          </div>

          <div className="space-y-2">
            <Label htmlFor="output-format">
              {t('admin.testCases.bulkUpload.outputFormat')}
            </Label>
            <Input
              id="output-format"
              value={outputFormat}
              onChange={(e) => setOutputFormat(e.target.value)}
              placeholder={DEFAULT_OUTPUT_FORMAT}
              required
            />
            <p className="text-xs text-muted-foreground">
              {t('admin.testCases.bulkUpload.formatHint')}
            </p>
          </div>

          <div className="space-y-2">
            <Label htmlFor="strategy">
              {t('admin.testCases.bulkUpload.strategy')}
            </Label>
            <Select
              value={strategy}
              onValueChange={(value) =>
                setStrategy(value as TestCaseMergeStrategy)
              }
            >
              <SelectTrigger id="strategy">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {TEST_CASE_MERGE_STRATEGIES.map((s) => (
                  <SelectItem key={s} value={s}>
                    {t(`admin.testCases.bulkUpload.strategy.${s}`)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>

          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => handleOpenChange(false)}
              disabled={loading}
            >
              {t('common.cancel')}
            </Button>
            <Button type="submit" disabled={loading || !selectedFile}>
              {loading
                ? t('admin.testCases.bulkUpload.uploading')
                : t('admin.testCases.bulkUpload.upload')}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
