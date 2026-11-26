import { formatList, pluginName, useTranslation } from '@broccoli/web-sdk/i18n';
import { Badge } from '@broccoli/web-sdk/ui';
import { CircleDot, TriangleAlert } from 'lucide-react';

import type { ServerReadiness } from '@/features/system/types';

interface Props {
  servers: ServerReadiness[];
  /** The servers serve different plugins: a misconfigured fleet. */
  mixed: boolean;
}

/**
 * Each live server: whether the gateway sends it browsers (`GET /readyz`),
 * and the plugins it lacks while another server serves them, which
 * `/readyz` itself never names. A fleet whose servers serve different
 * plugins is shown as misconfigured.
 */
export function ServerReadinessList({ servers, mixed }: Props) {
  const { t, lookup, locale } = useTranslation();

  if (servers.length === 0) {
    return (
      <p className="text-sm text-muted-foreground">
        {t('system.servers.empty')}
      </p>
    );
  }

  return (
    <div className="space-y-3">
      {mixed && (
        <p
          role="alert"
          className="flex items-start gap-2 rounded-md border border-destructive/40 bg-destructive/5 p-3 text-sm text-destructive"
        >
          <TriangleAlert className="mt-0.5 h-4 w-4 shrink-0" />
          {t('system.servers.mixed')}
        </p>
      )}
      <ul className="divide-y rounded-lg border bg-card">
        {servers.map((server) => (
          <li key={server.server_id} className="space-y-1 p-3">
            <div className="flex items-center justify-between gap-3">
              <span
                className="font-mono text-xs truncate"
                title={server.server_id}
              >
                {server.server_id}
              </span>
              {server.ready ? (
                <Badge
                  variant="outline"
                  className="gap-1 border-emerald-500/40"
                >
                  <CircleDot className="h-3 w-3 text-emerald-500" />
                  {t('system.servers.ready')}
                </Badge>
              ) : (
                <Badge
                  variant="outline"
                  className="gap-1 border-destructive/40 text-destructive"
                >
                  <CircleDot className="h-3 w-3" />
                  {t('system.servers.notReady')}
                </Badge>
              )}
            </div>
            {server.missing.length > 0 && (
              <p className="text-xs text-muted-foreground">
                {t('system.servers.missing', {
                  plugins: formatList(
                    server.missing.map((id) => pluginName(lookup, id)),
                    locale,
                  ),
                })}
              </p>
            )}
          </li>
        ))}
      </ul>
    </div>
  );
}
