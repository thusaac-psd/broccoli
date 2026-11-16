import { useServerNow } from '@broccoli/web-sdk/api';
import type { I18nContextValue } from '@broccoli/web-sdk/i18n';
import { useFormat, useTranslation } from '@broccoli/web-sdk/i18n';
import { Fragment } from 'react';
import { useParams } from 'react-router';

import { useContestInfo } from '@/features/contest/hooks/use-contest-info';

// -- helpers ------------------------------------------------------------------

function getTimeLeft(targetMs: number, now: number) {
  const totalSecs = Math.floor(Math.max(0, targetMs - now) / 1000);
  return {
    days: Math.floor(totalSecs / 86400),
    hours: Math.floor((totalSecs % 86400) / 3600),
    minutes: Math.floor((totalSecs % 3600) / 60),
    seconds: totalSecs % 60,
  };
}

/** The digit groups to show: days only when at least a day is left. */
function countdownSegments(
  tl: ReturnType<typeof getTimeLeft>,
  t: I18nContextValue['t'],
) {
  return [
    ...(tl.days > 0 ? [{ value: tl.days, label: t('countdown.days') }] : []),
    { value: tl.hours, label: t('countdown.hours') },
    { value: tl.minutes, label: t('countdown.minutes') },
    { value: tl.seconds, label: t('countdown.seconds') },
  ];
}

// CSS-variable-based color tokens - respond to any theme automatically
const ACCENT = 'hsl(var(--primary))';

// -- shared data hook ----------------------------------------------------------

function useCountdownData(contestId: number) {
  // The server enforces the window by its clock, so count down by it too.
  const now = useServerNow(1000);
  const { contest } = useContestInfo(
    Number.isFinite(contestId) && contestId > 0 ? contestId : NaN,
  );

  if (!contest) return null;

  const startMs = Date.parse(contest.start_time);
  const endMs = Date.parse(contest.end_time);
  // The server's phase; `useContestInfo` asks again once it is due to change.
  const phase =
    contest.phase === 'unactivated' || contest.phase === 'upcoming'
      ? 'upcoming'
      : contest.phase === 'running'
        ? 'running'
        : 'ended';

  const tl = getTimeLeft(phase === 'upcoming' ? startMs : endMs, now);
  const totalDuration = endMs - startMs;
  const elapsed = Math.max(0, now - startMs);
  const progress =
    phase === 'running' ? Math.min(100, (elapsed / totalDuration) * 100) : 0;

  return { contest, phase, tl, progress };
}

// -- full card -----------------------------------------------------------------

export function ContestCountdown() {
  const { contestId } = useParams();
  const { t } = useTranslation();
  const format = useFormat();
  const d = useCountdownData(Number(contestId));

  if (!d) return null;
  const { contest, phase, tl, progress } = d;

  const active = phase !== 'ended';
  const segments = countdownSegments(tl, t);

  const phaseLabel =
    phase === 'upcoming'
      ? t('countdown.startsIn')
      : phase === 'running'
        ? t('countdown.endsIn')
        : t('countdown.contestOver');

  return (
    <div className="rounded-lg border p-6 space-y-5">
      {/* Phase label */}
      <div className="flex items-center gap-1.5">
        {active && (
          <span
            className={`inline-block h-1.5 w-1.5 rounded-full ${phase === 'running' ? 'animate-pulse' : ''}`}
            style={{ backgroundColor: ACCENT }}
          />
        )}
        <span
          className="text-[10px] font-semibold uppercase tracking-[0.15em]"
          style={{ color: active ? ACCENT : undefined }}
        >
          {phaseLabel}
        </span>
      </div>

      {!active ? (
        <p className="text-sm text-muted-foreground">
          {t('countdown.finishedMessage')}
        </p>
      ) : (
        <>
          {/* Countdown digits */}
          <div className="flex items-end gap-2 justify-center">
            {segments.map((seg, i) => (
              <Fragment key={seg.label}>
                <div className="flex flex-col items-center">
                  <span
                    className="inline-flex min-w-[2ch] justify-center tabular-nums text-4xl font-bold leading-none tracking-tighter"
                    style={{ color: ACCENT }}
                  >
                    {String(seg.value).padStart(2, '0')}
                  </span>
                  <span className="mt-1 text-[8px] font-medium uppercase tracking-widest text-muted-foreground">
                    {seg.label}
                  </span>
                </div>
                {i < segments.length - 1 && (
                  <span className="mb-4 inline-flex w-3 select-none justify-center">
                    <span
                      className="text-xl font-extralight opacity-30"
                      style={{ color: ACCENT }}
                    >
                      :
                    </span>
                  </span>
                )}
              </Fragment>
            ))}
          </div>

          {/* Progress bar */}
          {phase === 'running' && (
            <div>
              <div className="w-full h-1 overflow-hidden rounded-full bg-foreground/5">
                <div
                  className="h-full rounded-full transition-none"
                  style={{
                    width: `${progress}%`,
                    backgroundColor: ACCENT,
                  }}
                />
              </div>
              <div className="mt-0.5 text-right text-[9px] tabular-nums text-muted-foreground">
                {Math.round(progress)}%
              </div>
            </div>
          )}
        </>
      )}

      {/* Schedule */}
      <div className="text-xs text-muted-foreground font-medium tabular-nums space-y-1">
        <div>
          <span className="text-muted-foreground mr-1.5">
            {t('contest.countdown.start')}
          </span>
          {format.dateTime(contest.start_time, { timeZoneName: true })}
        </div>
        <div>
          <span className="text-muted-foreground mr-1.5">
            {t('contest.countdown.end')}
          </span>
          {format.dateTime(contest.end_time, { timeZoneName: true })}
        </div>
      </div>
    </div>
  );
}

// -- mini (problem-detail top-right) ------------------------------------------

export function ContestCountdownMini() {
  const { contestId } = useParams();
  const { t } = useTranslation();
  const d = useCountdownData(Number(contestId));

  // Only render inside a contest route with live data
  if (!contestId || !d || d.phase === 'ended') return null;
  const { phase, tl } = d;

  const segments = countdownSegments(tl, t);

  const phaseLabel =
    phase === 'upcoming' ? t('countdown.startsIn') : t('countdown.endsIn');

  return (
    <div className="flex items-center">
      <div className="inline-flex items-center gap-2.5 px-2">
        {/* dot */}
        <span
          className={`h-1.5 w-1.5 shrink-0 rounded-full ${phase === 'running' ? 'animate-pulse' : ''}`}
          style={{ backgroundColor: ACCENT }}
        />
        {/* label */}
        <span
          className="text-[10px] font-semibold uppercase tracking-[0.15em]"
          style={{ color: ACCENT }}
        >
          {phaseLabel}
        </span>
        {/* digits */}
        <div className="flex items-center gap-1.5">
          {segments.map((seg, i) => (
            <Fragment key={seg.label}>
              <div className="flex flex-col items-center leading-none gap-1">
                <span
                  className="inline-flex min-w-[2ch] justify-center tabular-nums text-3xl font-bold tracking-tighter"
                  style={{ color: ACCENT }}
                >
                  {String(seg.value).padStart(2, '0')}
                </span>
                <span className="text-[8px] font-medium uppercase tracking-widest text-muted-foreground">
                  {seg.label}
                </span>
              </div>
              {i < segments.length - 1 && (
                <span className="mb-4 inline-flex w-3 select-none justify-center">
                  <span
                    className="text-xl font-light opacity-25"
                    style={{ color: ACCENT }}
                  >
                    :
                  </span>
                </span>
              )}
            </Fragment>
          ))}
        </div>
      </div>
    </div>
  );
}
