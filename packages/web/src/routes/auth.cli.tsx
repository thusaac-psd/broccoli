import { signInPath, useAuth } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import {
  Button,
  Card,
  CardContent,
  CardFooter,
  CardHeader,
  CardTitle,
} from '@broccoli/web-sdk/ui';
import { useEffect, useState } from 'react';
import { useLocation, useNavigate, useSearchParams } from 'react-router';

import { pageTitle, type RouteHandle } from '@/lib/route-title';

// Loopback-only redirect targets, else a crafted redirect_uri exfiltrates the token.
function isLoopbackRedirect(uri: string): boolean {
  try {
    const u = new URL(uri);
    return (
      u.protocol === 'http:' &&
      (u.hostname === 'localhost' ||
        u.hostname === '127.0.0.1' ||
        u.hostname === '[::1]')
    );
  } catch {
    return false;
  }
}

export const handle: RouteHandle = pageTitle('cliAuth.title');

export default function CliAuthPage() {
  const { t } = useTranslation();
  const { logout, accessToken, user, isLoading } = useAuth();
  const [searchParams] = useSearchParams();
  const location = useLocation();
  const navigate = useNavigate();

  const rawRedirectUri = searchParams.get('redirect_uri') ?? '';
  // Reject non-loopback redirect targets; fall back to paste-token mode.
  const redirectUri = isLoopbackRedirect(rawRedirectUri) ? rawRedirectUri : '';
  const redirectRejected = rawRedirectUri !== '' && redirectUri === '';
  const state = searchParams.get('state') ?? '';

  const [authorized, setAuthorized] = useState(false);

  const isAuthenticated = !!accessToken;
  const returnTo = `${location.pathname}${location.search}`;

  // Not signed in: delegate to the shared login page, returning here after.
  useEffect(() => {
    if (!isLoading && !isAuthenticated) {
      navigate(signInPath(returnTo), {
        replace: true,
      });
    }
  }, [isLoading, isAuthenticated, returnTo, navigate]);

  // After the user authorizes, redirect to the CLI's loopback callback.
  useEffect(() => {
    if (authorized && accessToken && redirectUri) {
      redirectWithToken(redirectUri, accessToken, state);
    }
  }, [authorized, accessToken, redirectUri, state]);

  const handleAuthorize = () => setAuthorized(true);

  const handleSwitchUser = async () => {
    try {
      await logout();
    } catch {
      // Ignore logout failures; the redirect below still shows the login form.
    }
    navigate(signInPath(returnTo), { replace: true });
  };

  if (isLoading || !isAuthenticated) {
    return (
      <div className="flex min-h-screen items-center justify-center px-4">
        <p className="text-sm text-muted-foreground">{t('common.loading')}</p>
      </div>
    );
  }

  if (authorized && !redirectUri && accessToken) {
    return (
      <div className="flex min-h-screen items-center justify-center px-4">
        <Card className="w-full max-w-md">
          <CardHeader>
            <CardTitle>{t('cliAuth.authorized')}</CardTitle>
          </CardHeader>
          <CardContent className="space-y-4">
            {user && (
              <p className="text-sm text-muted-foreground">
                {t('cliAuth.signedInAs', { username: user.username })}
              </p>
            )}
            <div className="rounded-md border bg-muted/50 p-4">
              <code className="break-all text-xs text-muted-foreground">
                {accessToken}
              </code>
            </div>
            <p className="text-sm text-muted-foreground">
              {t('cliAuth.pasteToken')}
            </p>
          </CardContent>
        </Card>
      </div>
    );
  }

  return (
    <div className="flex min-h-screen items-center justify-center px-4">
      <Card className="w-full max-w-md">
        <CardHeader>
          <CardTitle className="text-2xl">{t('cliAuth.title')}</CardTitle>
        </CardHeader>
        <CardContent className="space-y-4">
          {redirectRejected && (
            <p className="rounded-md border border-destructive/30 bg-destructive/10 p-3 text-sm text-destructive">
              {t('cliAuth.redirectBlocked')}
            </p>
          )}
          <p className="text-sm text-muted-foreground">
            {user
              ? t('cliAuth.requestFor', { username: user.username })
              : t('cliAuth.request')}
          </p>
          <p className="text-sm text-muted-foreground">{t('cliAuth.scope')}</p>
        </CardContent>
        <CardFooter className="flex-col gap-3">
          <Button className="w-full" onClick={handleAuthorize}>
            {t('cliAuth.authorize')}
          </Button>
          <Button
            variant="outline"
            className="w-full"
            onClick={handleSwitchUser}
          >
            {t('cliAuth.switchUser')}
          </Button>
        </CardFooter>
      </Card>
    </div>
  );
}

function redirectWithToken(uri: string, token: string, state: string) {
  const sep = uri.includes('?') ? '&' : '?';
  let url = `${uri}${sep}token=${encodeURIComponent(token)}`;
  if (state) {
    url += `&state=${encodeURIComponent(state)}`;
  }
  window.location.href = url;
}
