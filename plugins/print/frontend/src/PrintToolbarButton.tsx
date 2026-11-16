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
import { AlertCircle, Check, Loader2, Printer, Upload } from 'lucide-react';
import { useRef, useState } from 'react';
import { useParams } from 'react-router';

import { ApiError, usePrintApi } from './hooks/usePrintApi';
import { estimatePages } from './lib/format';

const LANGUAGES: { value: string; label: string }[] = [
  { value: 'cpp', label: 'C++' },
  { value: 'c', label: 'C' },
  { value: 'python3', label: 'Python' },
  { value: 'java', label: 'Java' },
  { value: 'kotlin', label: 'Kotlin' },
  { value: 'rust', label: 'Rust' },
  { value: 'go', label: 'Go' },
  { value: 'javascript', label: 'JavaScript' },
  { value: 'text', label: 'Plain text' },
];

type Phase = 'idle' | 'sending' | 'done' | 'error';

export function PrintToolbarButton() {
  const { t } = useTranslation();
  const api = usePrintApi();
  const params = useParams();
  const contestId = params.contestId ? Number(params.contestId) : undefined;
  const problemId = params.problemId ? Number(params.problemId) : undefined;

  const [open, setOpen] = useState(false);
  const [filename, setFilename] = useState('print.txt');
  const [language, setLanguage] = useState('cpp');
  const [source, setSource] = useState('');
  const [phase, setPhase] = useState<Phase>('idle');
  const [message, setMessage] = useState<string | null>(null);
  const fileInput = useRef<HTMLInputElement>(null);

  const pages = source.trim() ? estimatePages(source) : 0;

  const reset = () => {
    setPhase('idle');
    setMessage(null);
  };

  const onFile = async (file: File | undefined) => {
    if (!file) return;
    const text = await file.text();
    setSource(text);
    setFilename(file.name);
  };

  const submit = async () => {
    if (!source.trim()) {
      setPhase('error');
      setMessage(t('print.dialog.empty'));
      return;
    }
    setPhase('sending');
    setMessage(null);
    try {
      const r = await api.printArbitrary({
        contest_id: contestId,
        problem_id: problemId,
        filename: filename.trim() || 'print.txt',
        language,
        source,
      });
      setPhase('done');
      setMessage(
        r.status === 'pending_approval'
          ? t('print.action.needsApproval')
          : t('print.action.submitted'),
      );
      window.setTimeout(() => {
        setOpen(false);
        setSource('');
        reset();
      }, 2200);
    } catch (e) {
      setPhase('error');
      setMessage(e instanceof ApiError ? e.message : t('print.action.failed'));
    }
  };

  return (
    <>
      <Button
        variant="ghost"
        size="sm"
        className="gap-1.5"
        title={t('print.action.printCode')}
        onClick={() => {
          reset();
          setOpen(true);
        }}
      >
        <Printer className="h-4 w-4" />
        <span className="hidden sm:inline">{t('print.action.print')}</span>
      </Button>

      <Dialog
        open={open}
        onOpenChange={(o) => {
          setOpen(o);
          if (!o) reset();
        }}
      >
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
                <SelectTrigger className="w-36">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {LANGUAGES.map((l) => (
                    <SelectItem key={l.value} value={l.value}>
                      {l.label}
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

          {phase === 'error' && message && (
            <p className="flex items-center gap-2 text-sm text-red-600 dark:text-red-400">
              <AlertCircle className="h-4 w-4 shrink-0" />
              {message}
            </p>
          )}
          {phase === 'done' && message && (
            <p className="flex items-center gap-2 text-sm text-emerald-600 dark:text-emerald-400">
              <Check className="h-4 w-4 shrink-0" />
              {message}
            </p>
          )}

          <DialogFooter className="items-center sm:justify-between">
            <span className="text-xs text-muted-foreground tabular-nums">
              {pages > 0 ? `~${pages}p` : ''}
            </span>
            <div className="flex gap-2">
              <Button
                variant="ghost"
                onClick={() => setOpen(false)}
                disabled={phase === 'sending'}
              >
                {t('print.dialog.cancel')}
              </Button>
              <Button
                onClick={submit}
                disabled={
                  phase === 'sending' || phase === 'done' || !source.trim()
                }
                className="gap-1.5 min-w-24"
              >
                {phase === 'sending' && (
                  <Loader2 className="h-4 w-4 animate-spin" />
                )}
                {phase === 'done' && <Check className="h-4 w-4" />}
                {phase === 'sending'
                  ? t('print.action.printing')
                  : phase === 'done'
                    ? t('print.action.submitted')
                    : t('print.dialog.submit')}
              </Button>
            </div>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
