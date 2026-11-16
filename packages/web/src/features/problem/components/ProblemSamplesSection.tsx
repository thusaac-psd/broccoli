import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Button } from '@broccoli/web-sdk/ui';
import { formatBytes } from '@broccoli/web-sdk/utils';
import { Check, Copy } from 'lucide-react';

import { Markdown } from '@/components/Markdown';

type SampleContentMap = Record<number, { input?: string; output?: string }>;

type CopySampleHandler = (
  tcId: number,
  sampleIndex: number,
  type: 'input' | 'output',
  anchorEl: HTMLElement,
  inlineContent?: string,
) => Promise<void>;

type DownloadSampleHandler = (
  tcId: number,
  sampleIndex: number,
  type: 'input' | 'output',
) => Promise<void>;

interface ProblemSample {
  id: number;
  input_size: number;
  output_size: number;
  description?: string | null;
}

interface ProblemSamplesSectionProps {
  samples: ProblemSample[];
  sampleContents: SampleContentMap;
  copiedKey: string | null;
  onCopySample: CopySampleHandler;
  onDownloadSample: DownloadSampleHandler;
  /** Rewrites attachment URLs in sample descriptions (see `Markdown`). */
  resolveUrl?: (url: string | undefined) => string | undefined;
}

export function ProblemSamplesSection({
  samples,
  resolveUrl,
  sampleContents,
  copiedKey,
  onCopySample,
  onDownloadSample,
}: ProblemSamplesSectionProps) {
  const { t } = useTranslation();

  if (samples.length === 0) return null;

  return (
    <section className="mt-6 space-y-4">
      <h3 className="text-base font-bold">{t('problem.examples')}</h3>

      {samples.map((sample, index) => {
        const sampleNumber = index + 1;
        const sampleContent = sampleContents[sample.id] ?? {};

        return (
          <div key={sample.id} className="space-y-3">
            <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
              <div className="space-y-2">
                <div className="flex items-center justify-between px-1 text-sm font-medium">
                  {`${t('problem.input')} #${sampleNumber}`}
                  <Button
                    aria-label={t('problem.copy')}
                    type="button"
                    variant="ghost"
                    size="icon"
                    className="h-7 w-7"
                    title={t('problem.copy')}
                    onClick={(event) =>
                      onCopySample(
                        sample.id,
                        sampleNumber,
                        'input',
                        event.currentTarget,
                        sampleContent.input,
                      )
                    }
                  >
                    {copiedKey === `input-${sample.id}` ? (
                      <Check className="h-4 w-4" />
                    ) : (
                      <Copy className="h-4 w-4" />
                    )}
                  </Button>
                </div>
                <div className="overflow-hidden rounded-lg border">
                  {sampleContent.input !== undefined ? (
                    <pre className="mb-0 overflow-x-auto p-4 font-mono text-sm">
                      {sampleContent.input}
                    </pre>
                  ) : (
                    <div className="p-4 text-sm">
                      <button
                        type="button"
                        className="text-primary underline decoration-primary/40 underline-offset-2 transition-colors hover:decoration-primary/80"
                        onClick={() =>
                          onDownloadSample(sample.id, sampleNumber, 'input')
                        }
                      >
                        {t('problem.downloadSampleFile', {
                          file: `sample${sampleNumber}.in`,
                          size: formatBytes(sample.input_size),
                        })}
                      </button>
                    </div>
                  )}
                </div>
              </div>

              <div className="space-y-2">
                <div className="flex items-center justify-between px-1 text-sm font-medium">
                  {`${t('problem.output')} #${sampleNumber}`}
                  <Button
                    aria-label={t('problem.copy')}
                    type="button"
                    variant="ghost"
                    size="icon"
                    className="h-7 w-7"
                    title={t('problem.copy')}
                    onClick={(event) =>
                      onCopySample(
                        sample.id,
                        sampleNumber,
                        'output',
                        event.currentTarget,
                        sampleContent.output,
                      )
                    }
                  >
                    {copiedKey === `output-${sample.id}` ? (
                      <Check className="h-4 w-4" />
                    ) : (
                      <Copy className="h-4 w-4" />
                    )}
                  </Button>
                </div>
                <div className="overflow-hidden rounded-lg border">
                  {sampleContent.output !== undefined ? (
                    <pre className="mb-0 overflow-x-auto p-4 font-mono text-sm">
                      {sampleContent.output}
                    </pre>
                  ) : (
                    <div className="p-4 text-sm">
                      <button
                        type="button"
                        className="text-primary underline decoration-primary/40 underline-offset-2 transition-colors hover:decoration-primary/80"
                        onClick={() =>
                          onDownloadSample(sample.id, sampleNumber, 'output')
                        }
                      >
                        {t('problem.downloadSampleFile', {
                          file: `sample${sampleNumber}.out`,
                          size: formatBytes(sample.output_size),
                        })}
                      </button>
                    </div>
                  )}
                </div>
              </div>
            </div>
            {sample.description && (
              <div className="rounded-md border bg-muted/20 px-4 py-3">
                <Markdown resolveUrl={resolveUrl}>
                  {sample.description}
                </Markdown>
              </div>
            )}
          </div>
        );
      })}
    </section>
  );
}
