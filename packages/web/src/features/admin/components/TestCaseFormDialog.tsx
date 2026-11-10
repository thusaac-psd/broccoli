import { getErrorMessage, useApiClient } from '@broccoli/web-sdk/api';
import { useIdempotentRequest } from '@broccoli/web-sdk/hooks';
import { useTranslation } from '@broccoli/web-sdk/i18n';
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
  Separator,
  Textarea,
} from '@broccoli/web-sdk/ui';
import { useQueryClient } from '@tanstack/react-query';
import { Upload } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import { toast } from 'sonner';

import { SwitchField } from '@/features/admin/components/SwitchField';

interface TestCaseFormDialogProps {
  problemId: number;
  testCaseId?: number;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  testCasesQueryKey: (string | number)[];
}

type UploadType = 'Input' | 'Output';

export function TestCaseFormDialog({
  problemId,
  testCaseId,
  open,
  onOpenChange,
  testCasesQueryKey,
}: TestCaseFormDialogProps) {
  const { t } = useTranslation();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();
  const sendIdempotent = useIdempotentRequest();
  const isEdit = !!testCaseId;
  const fileInputRef = useRef<HTMLInputElement>(null);
  const fileOutputRef = useRef<HTMLInputElement>(null);

  const [label, setLabel] = useState('');
  const [input, setInput] = useState('');
  const [expectedOutput, setExpectedOutput] = useState('');
  const [score, setScore] = useState(0);
  const [isSample, setIsSample] = useState(false);
  const [description, setDescription] = useState('');
  const [loading, setLoading] = useState(false);
  const [loadingData, setLoadingData] = useState(false);
  const [inputUploaded, setInputUploaded] = useState(false);
  const [outputUploaded, setOutputUploaded] = useState(false);

  useEffect(() => {
    if (!open) return;
    if (testCaseId) {
      setLoadingData(true);
      apiClient
        .GET('/problems/{id}/test-cases/{tc_id}', {
          params: { path: { id: problemId, tc_id: testCaseId } },
        })
        .then(({ data, error }) => {
          setLoadingData(false);
          if (error || !data) return;
          setLabel(data.label ?? '');
          setInput(data.input);
          setExpectedOutput(data.expected_output);
          setScore(data.score);
          setIsSample(data.is_sample);
          setDescription(data.description ?? '');
        });
    } else {
      setLabel('');
      setInput('');
      setExpectedOutput('');
      setScore(0);
      setIsSample(false);
      setDescription('');
    }
  }, [apiClient, open, testCaseId, problemId]);

  const handleFileUpload = async (file: File) => {
    try {
      return await file.text();
    } catch {
      toast.error(t('admin.testCases.fileReadError'));
      return undefined;
    }
  };

  const handleFileChange = async (
    e: React.ChangeEvent<HTMLInputElement>,
    type: UploadType,
  ) => {
    const inputEl = e.currentTarget;
    const file = inputEl.files?.[0];
    if (!file) return;

    const content = await handleFileUpload(file);

    inputEl.value = '';

    if (!content) return;

    if (type === 'Input') {
      setInput(content);
      setInputUploaded(true);
    } else {
      setExpectedOutput(content);
      setOutputUploaded(true);
    }
  };

  const triggerFileUpload = (type: UploadType) => {
    console.log('Triggering file upload for', type);
    if (type === 'Input') {
      fileInputRef.current?.click();
    } else {
      fileOutputRef.current?.click();
    }
  };

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    setLoading(true);

    const createBody = {
      input,
      expected_output: expectedOutput,
      score,
      is_sample: isSample,
      description: description || null,
      label: label.trim(),
    };
    const updateBody = {
      input,
      expected_output: expectedOutput,
      score,
      is_sample: isSample,
      description: description || null,
      label: label.trim() || null,
    };

    const result = isEdit
      ? await apiClient.PATCH('/problems/{id}/test-cases/{tc_id}', {
          params: { path: { id: problemId, tc_id: testCaseId! } },
          body: updateBody,
        })
      : await sendIdempotent({ problemId, body: createBody }, (key) =>
          apiClient.POST('/problems/{id}/test-cases', {
            headers: { 'Idempotency-Key': key },
            params: { path: { id: problemId } },
            body: createBody,
          }),
        );

    setLoading(false);
    if (result.error) {
      toast.error(
        getErrorMessage(
          result.error,
          isEdit ? t('admin.editError') : t('admin.createError'),
        ),
      );
    } else {
      toast.success(
        isEdit ? t('toast.testCase.updated') : t('toast.testCase.created'),
      );
      queryClient.invalidateQueries({ queryKey: testCasesQueryKey });
      queryClient.invalidateQueries({ queryKey: ['problem', problemId] });
      queryClient.invalidateQueries({
        queryKey: ['problem-sample-contents', problemId],
      });
      onOpenChange(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-2xl max-h-[90vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>
            {isEdit ? t('admin.testCases.edit') : t('admin.testCases.create')}
          </DialogTitle>
          <DialogDescription>
            {isEdit ? '' : t('admin.testCases.createDesc')}
          </DialogDescription>
        </DialogHeader>

        {loadingData ? (
          <div className="py-8 text-center text-muted-foreground">
            {t('admin.loading')}
          </div>
        ) : (
          <form onSubmit={handleSubmit} className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="tc-input">
                {t('admin.testCases.field.input')}
              </Label>
              <div className="flex justify-end gap-2">
                <input
                  ref={fileInputRef}
                  type="file"
                  className="hidden"
                  onChange={(e) => handleFileChange(e, 'Input')}
                />
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => triggerFileUpload('Input')}
                  className="flex-1"
                >
                  <Upload className="h-4 w-4 mr-2" />
                  上传文件
                </Button>
              </div>
              {!inputUploaded || input?.length <= 100 ? (
                <Textarea
                  id="tc-input"
                  value={input}
                  onChange={(e) => setInput(e.target.value)}
                  rows={6}
                  className="font-mono text-sm"
                  placeholder="4&#10;2 7 11 15&#10;9"
                />
              ) : (
                <Button
                  type="button"
                  variant="outline"
                  className="w-full"
                  onClick={() => {
                    setInput('');
                    setInputUploaded(false);
                  }}
                >
                  已上传输入文件，内容已隐藏。长度：{input.length} 字符
                </Button>
              )}
            </div>

            <div className="space-y-2">
              <Label htmlFor="tc-expected-output">
                {t('admin.testCases.field.expectedOutput')}
              </Label>
              <div className="flex justify-end gap-2">
                <input
                  ref={fileOutputRef}
                  type="file"
                  className="hidden"
                  onChange={(e) => handleFileChange(e, 'Output')}
                />
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => triggerFileUpload('Output')}
                  className="flex-1"
                >
                  <Upload className="h-4 w-4 mr-2" />
                  上传文件
                </Button>
              </div>
              {!outputUploaded || expectedOutput?.length <= 100 ? (
                <Textarea
                  id="tc-expected-output"
                  value={expectedOutput}
                  onChange={(e) => setExpectedOutput(e.target.value)}
                  rows={6}
                  className="font-mono text-sm"
                  placeholder="0 1"
                />
              ) : (
                <Button
                  type="button"
                  variant="outline"
                  className="w-full"
                  onClick={() => {
                    setExpectedOutput('');
                    setOutputUploaded(false);
                  }}
                >
                  已上传输出文件，内容已隐藏。长度：{expectedOutput.length} 字符
                </Button>
              )}
            </div>

            <div className="grid grid-cols-1 gap-4 sm:grid-cols-3">
              <div className="space-y-2">
                <Label htmlFor="tc-label">
                  {t('admin.testCases.field.label')}
                </Label>
                <Input
                  id="tc-label"
                  value={label}
                  onChange={(e) => setLabel(e.target.value)}
                  maxLength={64}
                  required={!isEdit}
                  placeholder="sample_01"
                  className="font-mono"
                />
              </div>
              <div className="space-y-2">
                <Label htmlFor="tc-score">
                  {t('admin.testCases.field.score')}
                </Label>
                <Input
                  id="tc-score"
                  type="number"
                  min={0}
                  max={10000}
                  value={score}
                  onChange={(e) => setScore(Number(e.target.value))}
                  required
                />
              </div>
              <div className="space-y-2">
                <Label htmlFor="tc-description">
                  {t('admin.testCases.field.description')}
                </Label>
                <Textarea
                  id="tc-description"
                  value={description}
                  onChange={(e) => setDescription(e.target.value)}
                  maxLength={4096}
                  rows={3}
                  placeholder="Explains this sample (markdown supported)"
                />
              </div>
            </div>

            <Separator />

            <div className="space-y-3">
              <Label className="text-sm text-muted-foreground">
                {t('admin.field.options')}
              </Label>
              <SwitchField
                id="tc-is-sample"
                label={t('admin.testCases.field.isSample')}
                checked={isSample}
                onCheckedChange={setIsSample}
              />
            </div>

            <DialogFooter>
              <Button type="submit" disabled={loading}>
                {loading
                  ? t('admin.saving')
                  : isEdit
                    ? t('admin.edit')
                    : t('admin.testCases.create')}
              </Button>
            </DialogFooter>
          </form>
        )}
      </DialogContent>
    </Dialog>
  );
}
