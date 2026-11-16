import { useTranslation } from '@broccoli/web-sdk/i18n';
import type { ConfigFormSlotProps } from '@broccoli/web-sdk/slot';
import React from 'react';

interface LanguageTools {
  compiler?: string;
  interpreter?: string;
  flags?: string[];
}

/**
 * One preview line per language in the compilation config: its compiler (or
 * interpreter) and configured flags in front of the source file.
 */
function buildPreviews(values: Record<string, unknown>) {
  return Object.entries(values).map(([language, value]) => {
    const tools = (value ?? {}) as LanguageTools;
    const command = [
      tools.compiler ?? tools.interpreter,
      ...(tools.flags ?? []),
      '<source>',
    ]
      .filter(Boolean)
      .join(' ');
    return { key: `lang.${language}`, command };
  });
}

export function CompileInfoBanner({ values }: ConfigFormSlotProps) {
  const { t } = useTranslation();
  const previews = buildPreviews(values);

  return (
    <div className="rounded-md border border-border bg-muted/50 p-3 mb-3">
      <div className="text-xs font-medium text-muted-foreground mb-2">
        {t('lang.compileCommandPreview')}
      </div>
      <div className="space-y-1">
        {previews.map((p) => (
          <div key={p.key} className="flex items-baseline gap-2 text-xs">
            <span className="font-medium text-foreground w-20 shrink-0">
              {t(p.key)}
            </span>
            <code className="text-muted-foreground font-mono text-[11px] break-all">
              {p.command}
            </code>
          </div>
        ))}
      </div>
    </div>
  );
}
