/**
 * Wraps the `scoring` object section in a collapsible panel.
 * Uses the "wrap" slot position - receives children as the original field content.
 *
 * Receives: { children } (the default SchemaField rendering for scoring)
 */

import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Button } from '@broccoli/web-sdk/ui';
import { type ReactNode, useState } from 'react';

interface CollapsibleWrapperProps {
  children: ReactNode;
}

export function CollapsibleWrapper({ children }: CollapsibleWrapperProps) {
  const { t } = useTranslation();
  const [collapsed, setCollapsed] = useState(false);

  return (
    <div className="relative">
      <div className="absolute -top-1 right-0 z-10">
        <Button
          variant="outline"
          size="sm"
          type="button"
          onClick={() => setCollapsed((v) => !v)}
          className="text-[11px] opacity-70 hover:opacity-100"
        >
          {collapsed
            ? t('configUiTest.collapsible.expand')
            : t('configUiTest.collapsible.collapse')}
          <span className="ml-1 text-[10px] text-amber-600">
            {t('configUiTest.pluginBadge')}
          </span>
        </Button>
      </div>
      <div
        style={{
          transition: 'max-height 0.2s ease, opacity 0.2s ease',
          maxHeight: collapsed ? '0px' : '2000px',
          overflow: collapsed ? 'hidden' : 'visible',
          opacity: collapsed ? 0 : 1,
        }}
      >
        {children}
      </div>
      {collapsed && (
        <div className="rounded-lg border border-dashed border-muted-foreground p-4 text-center text-xs opacity-50">
          {t('configUiTest.collapsible.collapsedHint')}
        </div>
      )}
    </div>
  );
}
