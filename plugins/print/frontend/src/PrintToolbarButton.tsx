import { useErrorMessage } from '@broccoli/web-sdk/api';
import { useAsyncDialog } from '@broccoli/web-sdk/hooks';
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
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Textarea,
} from '@broccoli/web-sdk/ui';
import { AlertCircle, Loader2, Printer, Upload } from 'lucide-react';
import { useRef, useState } from 'react';
import { useParams } from 'react-router';

import { usePrintApi } from './hooks/usePrintApi';
import { announcePrinted, printFailure } from './lib/send';

/** Languages a print job can be highlighted as, by their proper names. */
const LANGUAGES: { value: string; name?: string }[] = [
  { value: 'cpp', name: 'C++' },
  { value: 'c', name: 'C' },
  { value: 'python3', name: 'Python' },
  { value: 'java', name: 'Java' },
  { value: 'kotlin', name: 'Kotlin' },
  { value: 'rust', name: 'Rust' },
  { value: 'go', name: 'Go' },
  { value: 'javascript', name: 'JavaScript' },
  { value: 'text' },
];

export function PrintToolbarButton() {
  const { t } = useTranslation();
  const errorMessage = useErrorMessage();
  const api = usePrintApi();
  const params = useParams();
  const contestId = params.contestId ? Number(params.contestId) : undefined;
  const problemId = params.problemId ? Number(params.problemId) : undefined;

  const dialog = useAsyncDialog();
  const [filename, setFilename] = useState('print.txt');
  const [language, setLanguage] = useState('cpp');
  const [source, setSource] = useState('');
  const fileInput = useRef<HTMLInputElement>(null);

  const onFile = async (file: File | undefined) => {
    if (!file) return;
    const text = await file.text();
    setSource(text);
    setFilename(file.name);
  };

  const submit = () => {
    if (contestId === undefined || !source.trim()) return;
    void dialog.submit(async () => {
      const result = await api.printArbitrary({
        contest_id: contestId,
        problem_id: problemId,
        filename: filename.trim() || 'print.txt',
        language,
        source,
      });
      announcePrinted(result, t);
      setSource('');
    });
  };

  // Pasted code prints only within a contest.
  if (contestId === undefined) return null;

  return (
    <>
      <Button
        variant="ghost"
        size="sm"
        className="gap-1.5"
        title={t('print.action.printCode')}
        onClick={() => dialog.onOpenChange(true)}
      >
        <Printer className="h-4 w-4" />
        <span className="hidden sm:inline">{t('print.action.print')}</span>
      </Button>

      <Dialog open={dialog.open} onOpenChange={dialog.onOpenChange}>
        <DialogContent className="sm:max-w-lg">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <Printer className="h-4.5 w-4.5 text-primary" />
              {t('print.dialog.title')}
            </DialogTitle>
            <DialogDescription>
              {t('print.dialog.description')}
            </DialogDescription>
          </DialogHeader>

          <div className="grid grid-cols-[1fr_auto] gap-3">
            <div className="space-y-1.5">
              <Label htmlFor="print-filename">
                {t('print.dialog.filename')}
              </Label>
              <Input
                id="print-filename"
                value={filename}
                onChange={(e) => setFilename(e.target.value)}
                className="font-mono text-sm"
              />
            </div>
            <div className="space-y-1.5">
              <Label>{t('print.dialog.language')}</Label>
              <Select value={language} onValueChange={setLanguage}>
                <SelectTrigger
                  aria-label={t('print.dialog.language')}
                  className="w-36"
                >
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {LANGUAGES.map((l) => (
                    <SelectItem key={l.value} value={l.value}>
                      {l.name ?? t('print.dialog.plainText')}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
          </div>

          <div className="space-y-1.5">
            <div className="flex items-center justify-between">
              <Label htmlFor="print-source">{t('print.dialog.source')}</Label>
              <Button
                type="button"
                variant="ghost"
                size="sm"
                className="h-7 gap-1.5 text-xs text-muted-foreground"
                onClick={() => fileInput.current?.click()}
              >
                <Upload className="h-3.5 w-3.5" />
                {t('print.dialog.dropFile')}
              </Button>
              <input
                ref={fileInput}
                type="file"
                className="hidden"
                onChange={(e) => onFile(e.target.files?.[0])}
              />
            </div>
            <Textarea
              id="print-source"
              value={source}
              onChange={(e) => setSource(e.target.value)}
              placeholder={t('print.dialog.sourcePlaceholder')}
              spellCheck={false}
              className="h-56 resize-none font-mono text-xs leading-relaxed"
            />
          </div>

          {dialog.state.status === 'failed' && (
            <p className="flex items-center gap-2 text-sm text-red-600 dark:text-red-400">
              <AlertCircle className="h-4 w-4 shrink-0" />
              {printFailure(dialog.state.error, t, errorMessage)}
            </p>
          )}

          <DialogFooter>
            <div className="flex gap-2">
              <Button
                variant="ghost"
                onClick={() => dialog.onOpenChange(false)}
                disabled={dialog.submitting}
              >
                {t('print.dialog.cancel')}
              </Button>
              <Button
                onClick={submit}
                disabled={dialog.submitting || !source.trim()}
                className="gap-1.5 min-w-24"
              >
                {dialog.submitting && (
                  <Loader2 className="h-4 w-4 animate-spin" />
                )}
                {dialog.submitting
                  ? t('print.action.printing')
                  : t('print.dialog.submit')}
              </Button>
            </div>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
