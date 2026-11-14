import {
  parseApiError,
  useApiClient,
  useErrorMessage,
} from '@broccoli/web-sdk/api';
import { signInPath, useAuth } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Button } from '@broccoli/web-sdk/ui';
import {
  type FormEvent,
  type KeyboardEvent,
  useEffect,
  useRef,
  useState,
} from 'react';
import { useNavigate, useSearchParams } from 'react-router';

function TerminalIcon({ className }: { className?: string }) {
  return (
    <svg
      className={className}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <polyline points="4 17 10 11 4 5" />
      <line x1="12" y1="19" x2="20" y2="19" />
    </svg>
  );
}

function CheckCircleIcon({ className }: { className?: string }) {
  return (
    <svg className={className} viewBox="0 0 24 24" fill="none">
      <circle
        cx="12"
        cy="12"
        r="10"
        stroke="currentColor"
        strokeWidth="1.5"
        className="animate-[draw-circle_0.6s_ease-out_forwards]"
        strokeDasharray="63"
        strokeDashoffset="63"
      />
      <path
        d="M8 12l3 3 5-6"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
        className="animate-[draw-check_0.3s_ease-out_0.5s_forwards]"
        strokeDasharray="20"
        strokeDashoffset="20"
      />
    </svg>
  );
}

function LinkIcon({ className }: { className?: string }) {
  return (
    <svg
      className={className}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <path d="M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71" />
      <path d="M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71" />
    </svg>
  );
}

const CODE_LENGTH = 8;
const SEGMENT_SIZE = 4;

/** The CLI binary's name, as typed in a terminal. */
const CLI_NAME = 'broccoli';
const CLI_LOGIN = `${CLI_NAME} login`;

const SUCCESS_KEYFRAMES = `
          @keyframes draw-circle {
            to { stroke-dashoffset: 0; }
          }
          @keyframes draw-check {
            to { stroke-dashoffset: 0; }
          }
          @keyframes success-fade-in {
            from { opacity: 0; transform: translateY(12px); }
            to { opacity: 1; transform: translateY(0); }
          }
          @keyframes success-pulse {
            0%, 100% { opacity: 0.4; }
            50% { opacity: 0.8; }
          }
        `;

const PAGE_KEYFRAMES = `
        @keyframes fade-up {
          from { opacity: 0; transform: translateY(16px); }
          to { opacity: 1; transform: translateY(0); }
        }
        @keyframes slot-in {
          from { opacity: 0; transform: scale(0.9) translateY(4px); }
          to { opacity: 1; transform: scale(1) translateY(0); }
        }
        @keyframes cursor-blink {
          0%, 100% { opacity: 1; }
          50% { opacity: 0; }
        }
        @keyframes shake {
          0%, 100% { transform: translateX(0); }
          20%, 60% { transform: translateX(-6px); }
          40%, 80% { transform: translateX(6px); }
        }
        @keyframes grid-fade {
          from { opacity: 0; }
          to { opacity: 0.03; }
        }
      `;

export function DeviceAuthPage() {
  const { t } = useTranslation();
  const errorMessage = useErrorMessage();
  const { user, isLoading } = useAuth();
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();
  const apiClient = useApiClient();

  const [codeChars, setCodeChars] = useState<string[]>(
    Array(CODE_LENGTH).fill(''),
  );
  const [error, setError] = useState('');
  const [isSubmitting, setIsSubmitting] = useState(false);
  const [isAuthorized, setIsAuthorized] = useState(false);
  const [step, setStep] = useState<'enter' | 'confirm'>('enter');
  const [focusedIndex, setFocusedIndex] = useState(0);
  const inputRefs = useRef<(HTMLInputElement | null)[]>([]);

  // Auto-fill from URL param
  useEffect(() => {
    const code = searchParams.get('code');
    if (code) {
      const clean = code.toUpperCase().replace(/[^A-Z0-9]/g, '');
      const chars = clean.slice(0, CODE_LENGTH).split('');
      while (chars.length < CODE_LENGTH) chars.push('');
      setCodeChars(chars);
      if (clean.length >= CODE_LENGTH) {
        setFocusedIndex(CODE_LENGTH - 1);
      }
    }
  }, [searchParams]);

  // Not signed in: delegate to the shared login page, returning here after.
  // Wait for the auth bootstrap (session refresh) before deciding, and only
  // navigate from an effect; navigating during render loops React updates.
  useEffect(() => {
    if (!isLoading && !user) {
      const search = searchParams.toString();
      const returnUrl = `/auth/device${search ? `?${search}` : ''}`;
      navigate(signInPath(returnUrl), {
        replace: true,
      });
    }
  }, [isLoading, user, searchParams, navigate]);

  if (isLoading || !user) {
    return (
      <div className="flex min-h-screen items-center justify-center bg-background px-4">
        <p className="text-sm text-muted-foreground">{t('common.loading')}</p>
      </div>
    );
  }

  const getUserCode = () => {
    const raw = codeChars.join('');
    if (raw.length <= SEGMENT_SIZE) return raw;
    return `${raw.slice(0, SEGMENT_SIZE)}-${raw.slice(SEGMENT_SIZE)}`;
  };

  const handleCharInput = (index: number, value: string) => {
    const char = value.toUpperCase().replace(/[^A-Z0-9]/g, '');
    if (!char) return;

    const newChars = [...codeChars];
    newChars[index] = char[0];
    setCodeChars(newChars);

    if (index < CODE_LENGTH - 1) {
      setFocusedIndex(index + 1);
      inputRefs.current[index + 1]?.focus();
    }
  };

  const handleKeyDown = (index: number, e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Backspace') {
      e.preventDefault();
      const newChars = [...codeChars];
      if (codeChars[index]) {
        newChars[index] = '';
        setCodeChars(newChars);
      } else if (index > 0) {
        newChars[index - 1] = '';
        setCodeChars(newChars);
        setFocusedIndex(index - 1);
        inputRefs.current[index - 1]?.focus();
      }
    } else if (e.key === 'ArrowLeft' && index > 0) {
      setFocusedIndex(index - 1);
      inputRefs.current[index - 1]?.focus();
    } else if (e.key === 'ArrowRight' && index < CODE_LENGTH - 1) {
      setFocusedIndex(index + 1);
      inputRefs.current[index + 1]?.focus();
    }
  };

  const handlePaste = (e: React.ClipboardEvent) => {
    e.preventDefault();
    const pasted = e.clipboardData
      .getData('text')
      .toUpperCase()
      .replace(/[^A-Z0-9]/g, '');
    const chars = pasted.slice(0, CODE_LENGTH).split('');
    while (chars.length < CODE_LENGTH) chars.push('');
    setCodeChars(chars);
    const lastFilledIndex = Math.min(pasted.length, CODE_LENGTH) - 1;
    if (lastFilledIndex >= 0) {
      setFocusedIndex(lastFilledIndex);
      inputRefs.current[lastFilledIndex]?.focus();
    }
  };

  // Step 1 -> 2: the code entry never authorizes on its own. It moves to an
  // explicit consent screen so a user tricked into typing an attacker's code
  // must still knowingly approve granting a device access to their account.
  const handleContinue = (e: FormEvent) => {
    e.preventDefault();
    if (!isCodeComplete) return;
    setError('');
    setStep('confirm');
  };

  // Step 2: the user has seen what they are granting and explicitly approves.
  const handleAuthorize = async () => {
    setError('');
    setIsSubmitting(true);

    try {
      const { error: apiError } = await apiClient.POST(
        '/auth/device-authorize',
        {
          body: { user_code: getUserCode() },
        },
      );

      if (apiError) {
        const errorBody = parseApiError(apiError);
        if (errorBody?.code === 'NOT_FOUND') {
          setError(t('auth.device.codeNotFound'));
        } else if (errorBody?.code === 'CONFLICT') {
          setError(t('auth.device.codeAlreadyUsed'));
        } else {
          setError(errorMessage(apiError, t('auth.device.error')));
        }
        // Send the user back to re-check the code they entered.
        setStep('enter');
      } else {
        setIsAuthorized(true);
      }
    } catch {
      setError(t('auth.device.error'));
      setStep('enter');
    } finally {
      setIsSubmitting(false);
    }
  };

  const isCodeComplete = codeChars.every((c) => c !== '');

  if (isAuthorized) {
    return (
      <div className="relative flex min-h-screen items-center justify-center overflow-hidden bg-background px-4">
        <style>{SUCCESS_KEYFRAMES}</style>

        {/* Subtle radial glow */}
        <div
          className="pointer-events-none absolute inset-0"
          style={{
            background:
              'radial-gradient(600px circle at 50% 40%, hsl(var(--primary) / 0.06), transparent 70%)',
          }}
        />

        <div className="relative z-10 flex flex-col items-center gap-6 text-center">
          <div className="text-primary">
            <CheckCircleIcon className="h-16 w-16" />
          </div>
          <div
            className="space-y-2"
            style={{ animation: 'success-fade-in 0.5s ease-out 0.7s both' }}
          >
            <h1 className="text-2xl font-semibold tracking-tight text-foreground">
              {t('auth.device.successTitle')}
            </h1>
            <p className="max-w-sm text-sm leading-relaxed text-muted-foreground">
              {t('auth.device.successMessage')}
            </p>
          </div>

          {/* Decorative terminal prompt */}
          <div
            className="mt-4 flex items-center gap-2 rounded-lg border border-border/50 bg-card px-4 py-2.5 font-mono text-xs text-muted-foreground"
            style={{ animation: 'success-fade-in 0.5s ease-out 1s both' }}
          >
            <span className="text-primary">$</span>
            <span>{CLI_NAME}</span>
            <span className="text-primary">
              {t('deviceAuth.promptAuthenticated')}
            </span>
            <span
              className="inline-block h-3.5 w-1.5 rounded-sm bg-primary"
              style={{ animation: 'success-pulse 1.2s ease-in-out infinite' }}
            />
          </div>
        </div>
      </div>
    );
  }

  if (step === 'confirm') {
    const permissions = user.permissions ?? [];
    return (
      <div className="relative flex min-h-screen items-center justify-center overflow-hidden bg-background px-4">
        <div
          className="pointer-events-none absolute inset-0"
          style={{
            background:
              'radial-gradient(600px circle at 50% 30%, hsl(var(--primary) / 0.05), transparent 70%)',
          }}
        />

        <div className="relative z-10 w-full max-w-md">
          <div className="rounded-xl border border-border/60 bg-card px-5 py-6 shadow-xs sm:px-7">
            <div className="mb-5 flex flex-col items-center gap-3 text-center">
              <div className="flex h-11 w-11 items-center justify-center rounded-lg border border-border/60 bg-card">
                <TerminalIcon className="h-5 w-5 text-primary" />
              </div>
              <h1 className="text-lg font-semibold tracking-tight text-foreground">
                {t('auth.device.confirmTitle')}
              </h1>
            </div>

            <div className="mb-4 rounded-lg border border-border/50 bg-background/50 px-4 py-3 text-center">
              <p className="text-xs text-muted-foreground">
                {t('auth.device.confirmIdentity')}
              </p>
              <p className="mt-1 font-mono text-base font-semibold text-foreground">
                {user.username}
              </p>
              <p className="mt-2 text-xs text-muted-foreground">
                {permissions.length > 0
                  ? t('auth.device.confirmPermissionsLabel')
                  : t('auth.device.confirmNoPermissions')}
              </p>
              {permissions.length > 0 && (
                <div className="mt-2 flex flex-wrap justify-center gap-1">
                  {permissions.map((p) => (
                    <span
                      key={p}
                      className="rounded-full border border-border/60 bg-muted px-2 py-0.5 font-mono text-[10px] text-muted-foreground"
                    >
                      {p}
                    </span>
                  ))}
                </div>
              )}
            </div>

            <div className="mb-5 flex items-start gap-2 rounded-lg bg-destructive/10 px-3 py-2.5">
              <svg
                className="mt-0.5 h-3.5 w-3.5 shrink-0 text-destructive"
                viewBox="0 0 16 16"
                fill="currentColor"
              >
                <path d="M8 1a7 7 0 100 14A7 7 0 008 1zm-.75 4a.75.75 0 011.5 0v3a.75.75 0 01-1.5 0V5zm.75 6.25a.75.75 0 100-1.5.75.75 0 000 1.5z" />
              </svg>
              <p className="text-xs leading-relaxed text-destructive">
                {t('auth.device.confirmWarning')}
              </p>
            </div>

            {error && (
              <div className="mb-4 text-center text-xs text-destructive">
                {error}
              </div>
            )}

            <div className="flex gap-2">
              <Button
                type="button"
                variant="outline"
                className="flex-1"
                disabled={isSubmitting}
                onClick={() => {
                  setError('');
                  setStep('enter');
                }}
              >
                {t('auth.device.confirmBack')}
              </Button>
              <Button
                type="button"
                className="flex-1"
                disabled={isSubmitting}
                onClick={handleAuthorize}
              >
                {isSubmitting ? (
                  <span className="flex items-center gap-2">
                    <svg
                      className="h-4 w-4 animate-spin"
                      viewBox="0 0 24 24"
                      fill="none"
                    >
                      <circle
                        className="opacity-25"
                        cx="12"
                        cy="12"
                        r="10"
                        stroke="currentColor"
                        strokeWidth="4"
                      />
                      <path
                        className="opacity-75"
                        fill="currentColor"
                        d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z"
                      />
                    </svg>
                    {t('auth.device.authorizing')}
                  </span>
                ) : (
                  t('auth.device.confirmAuthorize')
                )}
              </Button>
            </div>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="relative flex min-h-screen items-center justify-center overflow-hidden bg-background px-4">
      <style>{PAGE_KEYFRAMES}</style>

      {/* Background grid texture */}
      <div
        className="pointer-events-none absolute inset-0"
        style={{
          backgroundImage: `
            linear-gradient(hsl(var(--foreground)) 1px, transparent 1px),
            linear-gradient(90deg, hsl(var(--foreground)) 1px, transparent 1px)
          `,
          backgroundSize: '40px 40px',
          animation: 'grid-fade 1s ease-out forwards',
          opacity: 0,
        }}
      />

      {/* Radial accent glow */}
      <div
        className="pointer-events-none absolute inset-0"
        style={{
          background:
            'radial-gradient(600px circle at 50% 30%, hsl(var(--primary) / 0.05), transparent 70%)',
        }}
      />

      <div className="relative z-10 w-full max-w-md">
        {/* Header */}
        <div
          className="mb-8 flex flex-col items-center gap-4 text-center"
          style={{ animation: 'fade-up 0.6s ease-out both' }}
        >
          <div className="flex items-center gap-3">
            <div className="flex h-10 w-10 items-center justify-center rounded-lg border border-border/60 bg-card shadow-xs">
              <TerminalIcon className="h-5 w-5 text-primary" />
            </div>
            <LinkIcon className="h-4 w-4 text-muted-foreground" />
            <div className="flex h-10 w-10 items-center justify-center rounded-lg border border-border/60 bg-card shadow-xs">
              <svg
                className="h-5 w-5 text-primary"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="1.5"
                strokeLinecap="round"
                strokeLinejoin="round"
              >
                <rect x="3" y="3" width="18" height="18" rx="2" />
                <circle cx="12" cy="12" r="3" />
              </svg>
            </div>
          </div>

          <div className="space-y-1.5">
            <h1 className="text-xl font-semibold tracking-tight text-foreground">
              {t('auth.device.title')}
            </h1>
            <p className="text-sm leading-relaxed text-muted-foreground">
              {t('auth.device.instructions')}
            </p>
          </div>
        </div>

        {/* Code input card */}
        <div
          className="rounded-xl border border-border/60 bg-card px-4 py-6 shadow-xs sm:px-6"
          style={{
            animation: error
              ? 'shake 0.4s ease-out'
              : 'fade-up 0.6s ease-out 0.1s both',
          }}
        >
          <form onSubmit={handleContinue}>
            <label className="mb-3 block text-center text-xs font-medium uppercase tracking-wider text-muted-foreground">
              {t('auth.device.codeLabel')}
            </label>

            {/* Split code input */}
            <div className="mb-5 flex items-center justify-center gap-1">
              {codeChars.map((char, i) => (
                <div
                  key={i}
                  className="contents"
                  style={{
                    animation: `slot-in 0.3s ease-out ${0.2 + i * 0.04}s both`,
                  }}
                >
                  <input
                    ref={(el) => {
                      inputRefs.current[i] = el;
                    }}
                    type="text"
                    inputMode="text"
                    autoComplete="off"
                    maxLength={1}
                    value={char}
                    onChange={(e) => handleCharInput(i, e.target.value)}
                    onKeyDown={(e) => handleKeyDown(i, e)}
                    onFocus={() => setFocusedIndex(i)}
                    onPaste={i === 0 ? handlePaste : undefined}
                    autoFocus={i === 0}
                    className={`h-10 min-w-0 flex-1 rounded-lg border bg-background text-center font-mono text-base font-semibold tracking-wide transition-all duration-150 focus:outline-hidden sm:h-12 sm:max-w-10 sm:text-lg ${
                      focusedIndex === i
                        ? 'border-primary/80 shadow-[0_0_0_1px_hsl(var(--primary)/0.3)] ring-0'
                        : char
                          ? 'border-border text-foreground'
                          : 'border-border/50 text-muted-foreground'
                    }`}
                    aria-label={t('deviceAuth.codeDigit', { index: i + 1 })}
                  />
                  {i === SEGMENT_SIZE - 1 && (
                    <div className="mx-0.5 flex shrink-0 items-center sm:mx-1">
                      <span className="text-base font-light text-muted-foreground sm:text-lg">
                        —
                      </span>
                    </div>
                  )}
                </div>
              ))}
            </div>

            {/* Error message */}
            {error && (
              <div className="mb-4 flex items-start gap-2 rounded-lg bg-destructive/10 px-3 py-2.5">
                <svg
                  className="mt-0.5 h-3.5 w-3.5 shrink-0 text-destructive"
                  viewBox="0 0 16 16"
                  fill="currentColor"
                >
                  <path d="M8 1a7 7 0 100 14A7 7 0 008 1zm-.75 4a.75.75 0 011.5 0v3a.75.75 0 01-1.5 0V5zm.75 6.25a.75.75 0 100-1.5.75.75 0 000 1.5z" />
                </svg>
                <p className="text-xs leading-relaxed text-destructive">
                  {error}
                </p>
              </div>
            )}

            <Button type="submit" className="w-full" disabled={!isCodeComplete}>
              {t('auth.device.continue')}
            </Button>
          </form>
        </div>

        {/* Footer hint */}
        <div
          className="mt-5 flex items-center justify-center gap-2 text-center"
          style={{ animation: 'fade-up 0.6s ease-out 0.3s both' }}
        >
          <div className="flex items-center gap-1.5 rounded-full border border-border/40 bg-card/50 px-3 py-1.5 font-mono text-[11px] text-muted-foreground">
            <span className="text-primary">$</span>
            <span>{CLI_LOGIN}</span>
            <span
              className="inline-block h-3 w-0.5 rounded-full bg-muted-foreground/30"
              style={{
                animation: 'cursor-blink 1.2s step-end infinite',
              }}
            />
          </div>
        </div>
      </div>
    </div>
  );
}
