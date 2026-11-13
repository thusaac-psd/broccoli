/**
 * Cooldown rejection wrapper for the `submission-result.rejection` slot.
 */
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Badge } from '@broccoli/web-sdk/ui';
import {
  type ReactNode,
  useCallback,
  useEffect,
  useRef,
  useState,
} from 'react';

interface Props {
  error?: { code: string; message: string; details?: Record<string, unknown> };
  children?: ReactNode;
}

const RING_RADIUS = 40;
const RING_CIRCUMFERENCE = 2 * Math.PI * RING_RADIUS;

function getCooldownSeconds(details?: Record<string, unknown>): number | null {
  if (!details) return null;
  const val = details.remaining_seconds;
  return typeof val === 'number' && val > 0 ? val : null;
}

export function CooldownRejection({ error, children }: Props) {
  if (error?.code !== 'COOLDOWN_ACTIVE') {
    return <>{children}</>;
  }

  return <CooldownCountdown details={error.details} />;
}

function CooldownCountdown({ details }: { details?: Record<string, unknown> }) {
  const { t } = useTranslation();
  const initialSeconds = getCooldownSeconds(details);
  const [remaining, setRemaining] = useState(initialSeconds ?? 0);
  const [expired, setExpired] = useState(initialSeconds === null);
  const startTimeRef = useRef(performance.now());
  const initialRef = useRef(initialSeconds ?? 0);

  const tick = useCallback(() => {
    const elapsed = (performance.now() - startTimeRef.current) / 1000;
    const left = Math.max(0, Math.ceil(initialRef.current - elapsed));
    setRemaining(left);
    if (left <= 0) setExpired(true);
  }, []);

  useEffect(() => {
    if (initialSeconds === null || initialSeconds <= 0) {
      setExpired(true);
      return;
    }
    startTimeRef.current = performance.now();
    initialRef.current = initialSeconds;
    setRemaining(initialSeconds);
    setExpired(false);

    const id = setInterval(tick, 200);
    return () => clearInterval(id);
  }, [initialSeconds, tick]);

  const progress = initialRef.current > 0 ? remaining / initialRef.current : 0;
  const dashOffset = RING_CIRCUMFERENCE * (1 - progress);
  const pct = (1 - progress) * 100;

  return (
    <div className="rounded-xl border border-border bg-card overflow-hidden h-full">
      {/* Header */}
      <div className="px-6 pt-5">
        <div className="flex items-center justify-between">
          <span className="text-base font-semibold text-foreground">
            {t('cooldown.result')}
          </span>
          <Badge variant="secondary" className="rounded-full">
            {t('cooldown.cooldown')}
          </Badge>
        </div>
      </div>

      {/* Content */}
      <div className="px-6 pb-6 pt-2 flex flex-col items-center gap-5">
        {/* Countdown ring */}
        <div className="relative">
          <svg
            width="100"
            height="100"
            viewBox="0 0 100 100"
            style={{ transform: 'rotate(-90deg)' }}
          >
            <circle
              cx="50"
              cy="50"
              r={RING_RADIUS}
              fill="none"
              stroke="var(--muted, #f1f5f9)"
              strokeWidth="5"
            />
            <circle
              cx="50"
              cy="50"
              r={RING_RADIUS}
              fill="none"
              stroke={expired ? '#10b981' : '#f59e0b'}
              strokeWidth="5"
              strokeLinecap="round"
              strokeDasharray={RING_CIRCUMFERENCE}
              strokeDashoffset={expired ? 0 : dashOffset}
              style={{
                transition: 'stroke-dashoffset 0.3s linear, stroke 0.3s ease',
              }}
            />
          </svg>
          <div className="absolute inset-0 flex items-center justify-center">
            {expired ? (
              <span className="text-[28px] text-emerald-500">&#10003;</span>
            ) : (
              <span className="font-mono tabular-nums text-2xl font-bold text-foreground">
                {remaining}
              </span>
            )}
          </div>
        </div>

        {/* Label */}
        <div className="text-center max-w-[260px]">
          {expired ? (
            <>
              <p className="text-[13px] font-medium text-emerald-500">
                {t('cooldown.readyToSubmit')}
              </p>
              <p className="text-[11px] text-muted-foreground mt-1">
                {t('cooldown.canSubmitNow')}
              </p>
            </>
          ) : (
            <>
              <p className="text-[13px] font-medium text-foreground">
                {t('cooldown.cooldownActive')}
              </p>
              <p className="text-[11px] text-muted-foreground mt-1">
                {t('cooldown.waitSeconds', { seconds: remaining })}
              </p>
            </>
          )}
        </div>

        {/* Progress bar */}
        {!expired && initialRef.current > 0 && (
          <div className="w-full max-w-[200px]">
            <div className="h-1 rounded-sm bg-muted overflow-hidden">
              <div
                className="h-full rounded-sm bg-amber-500"
                style={{ width: `${pct}%`, transition: 'width 0.3s linear' }}
              />
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
