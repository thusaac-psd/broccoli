import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Button, Input, Label } from '@broccoli/web-sdk/ui';
import { cn } from '@broccoli/web-sdk/utils';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { Plus, Shuffle, Sparkles, X } from 'lucide-react';
import { type ReactNode, useState } from 'react';

import { useBracketApi } from './hooks/useBracketApi';
import { useContestProblems, useParticipants } from './hooks/useContestData';
import { roundNameKey } from './lib/rounds';
import {
  autoFillRounds,
  emptyRound,
  PLAYER_COUNT,
  type RoundDraft,
  type SetupIssue,
  validateSetup,
} from './lib/setup';
import { ConfirmButton } from './parts';
import { SortableList } from './SortableList';
import type { SetupResponse } from './types';

/** The same editor handles initial setup and future-stage corrections. */
export function SetupPanel({
  contestId,
  onClose,
}: {
  contestId: number;
  onClose?: () => void;
}) {
  const api = useBracketApi();
  const config = useQuery({
    queryKey: ['codelink-bracket-setup', contestId],
    queryFn: () => api.getSetup(contestId),
    refetchOnWindowFocus: false,
  });
  if (config.isError) return <p role="alert">{config.error.message}</p>;
  if (!config.data) return null;
  return (
    <SetupForm
      key={config.dataUpdatedAt}
      contestId={contestId}
      initial={config.data}
      onClose={onClose}
    />
  );
}

function SetupForm({
  contestId,
  initial,
  onClose,
}: {
  contestId: number;
  initial: SetupResponse;
  onClose?: () => void;
}) {
  const { t } = useTranslation();
  const api = useBracketApi();
  const queryClient = useQueryClient();
  const participants = useParticipants(contestId, true);
  const { list: problemList, byId: problems } = useContestProblems(contestId);

  const [seeds, setSeeds] = useState<number[] | null>(
    initial.setup?.seeds ?? null,
  );
  const [rounds, setRounds] = useState<RoundDraft[] | null>(
    initial.setup?.rounds.map((r) => ({
      groupA: r.group_a,
      groupB: r.group_b,
      tiebreak: r.tiebreak,
      gameMinutes: r.xiaoju_seconds / 60,
      tiebreakMinutes: r.tiebreak_seconds / 60,
      breakMinutes: r.break_before_seconds / 60,
    })) ?? null,
  );
  const [graceSeconds, setGraceSeconds] = useState(
    initial.setup?.escalation_grace_seconds ?? 120,
  );
  const [penaltySeconds, setPenaltySeconds] = useState(
    initial.setup?.penalty_seconds ?? 20,
  );
  const [extraMinutes, setExtraMinutes] = useState(
    (initial.setup?.match_extra_time_seconds ?? 0) / 60,
  );
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [playerCount, setPlayerCount] = useState(
    initial.setup?.seeds.length ?? PLAYER_COUNT,
  );
  const stageCount = Math.log2(playerCount);
  const opened = initial.setup?.opened_rounds ?? 0;
  const structureLocked = initial.started || opened > 0;
  const people = participants.data ?? [];
  const nameOf = new Map(people.map((p) => [p.user_id, p.username]));
  // Until staff touch them, seeds follow registration order and problems
  // follow the contest's problem order.
  const seedList =
    seeds ??
    [...people]
      .sort((a, b) => a.registered_at.localeCompare(b.registered_at))
      .slice(0, playerCount)
      .map((p) => p.user_id);
  const roundList =
    rounds ??
    (problemList.length > 0
      ? autoFillRounds(
          problemList.map((p) => p.problem_id),
          undefined,
          stageCount,
        )
      : Array.from({ length: stageCount }, (_, i) => emptyRound(i)));
  const bench = people.filter((p) => !seedList.includes(p.user_id));
  const issues = validateSetup(
    seedList,
    roundList,
    penaltySeconds,
    graceSeconds,
    extraMinutes,
    playerCount,
  );

  const setRound = (i: number, next: RoundDraft) =>
    setRounds(roundList.map((r, j) => (j === i ? next : r)));

  const create = async () => {
    setError(null);
    setSaving(true);
    try {
      await api.setup(contestId, {
        seeds: seedList,
        rounds: roundList.map((r) => ({
          group_a: r.groupA as number[],
          group_b: r.groupB as number[],
          tiebreak: r.tiebreak.filter((id): id is number => id !== null),
          xiaoju_seconds: Math.round(r.gameMinutes * 60),
          tiebreak_seconds: Math.round(r.tiebreakMinutes * 60),
          break_before_seconds: Math.round(r.breakMinutes * 60),
        })),
        escalation_grace_seconds: Math.round(graceSeconds),
        penalty_seconds: penaltySeconds,
        match_extra_time_seconds: Math.round(extraMinutes * 60),
      });
      await queryClient.invalidateQueries({
        queryKey: ['codelink-bracket-setup', contestId],
      });
      onClose?.();
      await queryClient.invalidateQueries({
        queryKey: ['codelink-bracket-bracket', contestId],
      });
    } catch (err) {
      setError(
        err instanceof Error ? err.message : t('codelink-bracket.setup.failed'),
      );
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="space-y-5">
      <div>
        <h2 className="text-lg font-semibold">
          {t('codelink-bracket.setup.title')}
        </h2>
        <p className="text-sm text-muted-foreground">
          {t('codelink-bracket.setup.subtitle')}
        </p>
      </div>

      <Label htmlFor="bracket-player-count">
        {t('codelink-bracket.setup.size')}
      </Label>
      <select
        id="bracket-player-count"
        disabled={structureLocked}
        value={playerCount}
        className="ml-3 rounded border border-input bg-background p-2"
        onChange={(e) => {
          const count = Number(e.target.value);
          setPlayerCount(count);
          setSeeds(
            [...seedList, ...bench.map((p) => p.user_id)].slice(0, count),
          );
          setRounds(
            Array.from(
              { length: Math.log2(count) },
              (_, i) => roundList[i] ?? emptyRound(i),
            ),
          );
        }}
      >
        {[2, 4, 8, 16, 32, 64, 128].map((count) => (
          <option key={count} value={count}>
            {count}
          </option>
        ))}
      </select>
      {structureLocked && (
        <p className="text-sm text-muted-foreground">
          {t('codelink-bracket.setup.locked')}
        </p>
      )}
      <div className="grid gap-5 lg:grid-cols-[minmax(0,2fr)_minmax(0,3fr)]">
        <fieldset disabled={structureLocked}>
          <Panel
            title={t('codelink-bracket.setup.players', {
              count: seedList.length,
              total: playerCount,
            })}
            action={
              <Button
                size="sm"
                variant="ghost"
                onClick={() =>
                  setSeeds([...seedList].sort(() => Math.random() - 0.5))
                }
              >
                <Shuffle className="mr-1.5 h-3.5 w-3.5" />
                {t('codelink-bracket.setup.shuffle')}
              </Button>
            }
          >
            <p className="mb-3 text-xs text-muted-foreground">
              {t('codelink-bracket.setup.seedHint')}
            </p>
            <SortableList
              disabled={structureLocked}
              items={seedList}
              getKey={(id) => id}
              onChange={setSeeds}
              renderItem={(id, index) => (
                <div className="flex items-center gap-2">
                  <span className="truncate font-medium">
                    {nameOf.get(id) ?? `#${id}`}
                  </span>
                  {index % 2 === 0 && (
                    <span className="ml-auto shrink-0 text-[11px] text-muted-foreground">
                      {t('codelink-bracket.match.number', { n: index / 2 + 1 })}
                    </span>
                  )}
                  <button
                    type="button"
                    onClick={() => setSeeds(seedList.filter((s) => s !== id))}
                    className={cn(
                      'shrink-0 rounded p-0.5 text-muted-foreground hover:bg-accent hover:text-foreground',
                      index % 2 !== 0 && 'ml-auto',
                    )}
                    aria-label={t('codelink-bracket.setup.remove')}
                  >
                    <X className="h-3.5 w-3.5" />
                  </button>
                </div>
              )}
            />
            {bench.length > 0 && (
              <div className="mt-4 space-y-2">
                <div className="text-xs font-medium text-muted-foreground">
                  {t('codelink-bracket.setup.bench', { count: bench.length })}
                </div>
                <div className="flex flex-wrap gap-1.5">
                  {bench.map((p) => (
                    <button
                      key={p.user_id}
                      type="button"
                      disabled={seedList.length >= playerCount}
                      onClick={() => setSeeds([...seedList, p.user_id])}
                      className="inline-flex items-center gap-1 rounded-full border border-border px-2.5 py-1 text-xs hover:border-primary/50 hover:bg-accent disabled:opacity-40"
                    >
                      <Plus className="h-3 w-3" />
                      {p.username}
                    </button>
                  ))}
                </div>
              </div>
            )}
            {participants.isError && (
              <p className="mt-3 text-sm text-destructive">
                {t('codelink-bracket.setup.participantsFailed')}
              </p>
            )}
          </Panel>
        </fieldset>

        <div className="space-y-5">
          <Panel
            title={t('codelink-bracket.setup.problems')}
            action={
              // Only ask once there are hand-picked slots to lose.
              rounds === null ? null : (
                <ConfirmButton
                  size="sm"
                  variant="ghost"
                  title={t('codelink-bracket.confirm.autofillTitle')}
                  description={t(
                    'codelink-bracket.confirm.autofillDescription',
                  )}
                  confirmLabel={t('codelink-bracket.setup.autofill')}
                  onConfirm={() =>
                    setRounds(
                      autoFillRounds(
                        problemList.map((p) => p.problem_id),
                        roundList,
                        stageCount,
                        opened,
                      ),
                    )
                  }
                >
                  <Sparkles className="mr-1.5 h-3.5 w-3.5" />
                  {t('codelink-bracket.setup.autofill')}
                </ConfirmButton>
              )
            }
          >
            <p className="mb-3 text-xs text-muted-foreground">
              {t('codelink-bracket.setup.problemsHint')}
            </p>
            <div className="space-y-3">
              {roundList.map((r, i) => (
                <fieldset
                  disabled={i < opened}
                  key={i}
                  className="rounded-lg border border-border p-3 disabled:opacity-60"
                >
                  <div className="mb-2 text-xs font-semibold uppercase tracking-wide text-muted-foreground">
                    {t(roundNameKey(i + 1, stageCount))}
                  </div>
                  <NumberField
                    id={`bracket-games-${i}`}
                    label={t('codelink-bracket.setup.games')}
                    value={r.groupA.length}
                    onChange={(count) => {
                      if (
                        !Number.isInteger(count) ||
                        count < 1 ||
                        count + r.tiebreak.length > 256
                      )
                        return;
                      const resize = (group: (number | null)[]) =>
                        Array.from(
                          { length: count },
                          (_, j) => group[j] ?? null,
                        );
                      setRound(i, {
                        ...r,
                        groupA: resize(r.groupA),
                        groupB: resize(r.groupB),
                      });
                    }}
                  />
                  <div className="mt-3 grid gap-2 sm:grid-cols-[auto_1fr] sm:items-center">
                    <SlotRow
                      label={t('codelink-bracket.setup.firstPlayer')}
                      slots={r.groupA}
                      problems={problemList}
                      onChange={(groupA) => setRound(i, { ...r, groupA })}
                    />
                    <SlotRow
                      label={t('codelink-bracket.setup.secondPlayer')}
                      slots={r.groupB}
                      problems={problemList}
                      onChange={(groupB) => setRound(i, { ...r, groupB })}
                    />
                    <SlotRow
                      label={t('codelink-bracket.setup.tiebreak')}
                      slots={r.tiebreak}
                      problems={problemList}
                      onChange={(tiebreak) => setRound(i, { ...r, tiebreak })}
                      onAdd={
                        r.groupA.length + r.tiebreak.length >= 256
                          ? undefined
                          : () =>
                              setRound(i, {
                                ...r,
                                tiebreak: [...r.tiebreak, null],
                              })
                      }
                    />
                  </div>
                  <div className="mt-3 grid gap-3 sm:grid-cols-3">
                    <NumberField
                      id={`bracket-game-minutes-${i}`}
                      label={t('codelink-bracket.setup.gameMinutes')}
                      value={r.gameMinutes}
                      onChange={(gameMinutes) =>
                        setRound(i, { ...r, gameMinutes })
                      }
                    />
                    <NumberField
                      id={`bracket-tiebreak-minutes-${i}`}
                      label={t('codelink-bracket.setup.tiebreakMinutes')}
                      value={r.tiebreakMinutes}
                      onChange={(tiebreakMinutes) =>
                        setRound(i, { ...r, tiebreakMinutes })
                      }
                    />
                    {i > 0 && (
                      <NumberField
                        id={`bracket-break-minutes-${i}`}
                        label={t('codelink-bracket.setup.breakMinutes')}
                        value={r.breakMinutes}
                        onChange={(breakMinutes) =>
                          setRound(i, { ...r, breakMinutes })
                        }
                      />
                    )}
                  </div>
                </fieldset>
              ))}
            </div>
          </Panel>

          <fieldset disabled={structureLocked}>
            <Panel title={t('codelink-bracket.setup.timing')}>
              <div className="grid gap-3 sm:grid-cols-2">
                <NumberField
                  id="bracket-extra-minutes"
                  label={t('codelink-bracket.setup.extraMinutes')}
                  value={extraMinutes}
                  onChange={setExtraMinutes}
                />
                <NumberField
                  id="bracket-grace-seconds"
                  label={t('codelink-bracket.setup.graceSeconds')}
                  value={graceSeconds}
                  onChange={setGraceSeconds}
                />
                <NumberField
                  id="bracket-penalty-seconds"
                  label={t('codelink-bracket.setup.penaltySeconds')}
                  value={penaltySeconds}
                  onChange={setPenaltySeconds}
                />
              </div>
              <p className="mt-3 text-xs text-muted-foreground">
                {t('codelink-bracket.setup.extraHint')}
              </p>
            </Panel>
          </fieldset>
        </div>
      </div>

      <div className="sticky bottom-0 flex flex-wrap items-center gap-3 rounded-xl border border-border bg-card/95 px-5 py-3 shadow-sm backdrop-blur">
        <div className="min-w-0 flex-1 text-sm">
          {issues.length === 0 ? (
            <span className="text-emerald-700 dark:text-emerald-300">
              {t('codelink-bracket.setup.ready')}
            </span>
          ) : (
            <ul className="space-y-0.5 text-amber-800 dark:text-amber-300">
              {issues.slice(0, 3).map((issue, i) => (
                <li key={i}>
                  {describeIssue(issue, t, problems, stageCount, playerCount)}
                </li>
              ))}
            </ul>
          )}
          {error && <p className="mt-1 text-destructive">{error}</p>}
        </div>
        {initial.setup && onClose && (
          <Button variant="outline" onClick={onClose}>
            {t('codelink-bracket.setup.cancel')}
          </Button>
        )}
        <ConfirmButton
          disabled={issues.length > 0 || saving}
          title={t(
            initial.setup
              ? 'codelink-bracket.setup.edit'
              : 'codelink-bracket.setup.confirmTitle',
          )}
          description={t('codelink-bracket.setup.confirmDescription')}
          confirmLabel={t(
            initial.setup
              ? 'codelink-bracket.setup.save'
              : 'codelink-bracket.setup.create',
          )}
          onConfirm={() => void create()}
        >
          {saving
            ? t('codelink-bracket.setup.creating')
            : t(
                initial.setup
                  ? 'codelink-bracket.setup.save'
                  : 'codelink-bracket.setup.create',
              )}
        </ConfirmButton>
      </div>
    </div>
  );
}

function describeIssue(
  issue: SetupIssue,
  t: (key: string, params?: Record<string, string | number>) => string,
  problems: ReadonlyMap<number, { label: string }>,
  stageCount: number,
  playerCount: number,
): string {
  switch (issue.kind) {
    case 'players':
      return t('codelink-bracket.setup.issue.players', {
        count: issue.count,
        total: playerCount,
      });
    case 'emptySlot':
      return t('codelink-bracket.setup.issue.emptySlot', {
        round: t(roundNameKey(issue.round, stageCount)),
      });
    case 'duplicate':
      return t('codelink-bracket.setup.issue.duplicate', {
        label: problems.get(issue.problemId)?.label ?? `#${issue.problemId}`,
      });
    case 'badTiming':
      return t('codelink-bracket.setup.issue.badTiming', {
        round: t(roundNameKey(issue.round, stageCount)),
      });
    case 'badPenalty':
      return t('codelink-bracket.setup.issue.badPenalty');
    case 'badExtra':
      return t('codelink-bracket.setup.issue.badExtra');
    case 'badGrace':
      return t('codelink-bracket.setup.issue.badGrace');
  }
}

function Panel({
  title,
  action,
  children,
}: {
  title: string;
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="rounded-xl border border-border bg-card p-4">
      <div className="mb-2 flex items-center justify-between gap-2">
        <h3 className="text-sm font-semibold">{title}</h3>
        {action}
      </div>
      {children}
    </section>
  );
}

function SlotRow({
  label,
  slots,
  problems,
  onChange,
  onAdd,
}: {
  label: string;
  slots: (number | null)[];
  problems: { problem_id: number; label: string; problem_title: string }[];
  onChange: (slots: (number | null)[]) => void;
  onAdd?: () => void;
}) {
  const { t } = useTranslation();
  return (
    <>
      <span className="text-xs text-muted-foreground">{label}</span>
      <div className="flex flex-wrap items-center gap-1.5">
        {slots.map((value, i) => (
          <select
            key={i}
            value={value ?? ''}
            onChange={(e) =>
              onChange(
                slots.map((s, j) =>
                  j === i
                    ? e.target.value === ''
                      ? null
                      : Number(e.target.value)
                    : s,
                ),
              )
            }
            className={cn(
              'h-8 rounded-md border border-input bg-background px-2 font-mono text-xs',
              value === null && 'border-dashed text-muted-foreground',
            )}
          >
            <option value="">{t('codelink-bracket.setup.pick')}</option>
            {problems.map((p) => (
              <option key={p.problem_id} value={p.problem_id}>
                {p.label} · {p.problem_title}
              </option>
            ))}
          </select>
        ))}
        {onAdd && (
          <button
            type="button"
            onClick={onAdd}
            className="inline-flex h-8 items-center rounded-md border border-dashed border-border px-2 text-xs text-muted-foreground hover:text-foreground"
            aria-label={t('codelink-bracket.setup.addTiebreak')}
          >
            <Plus className="h-3.5 w-3.5" />
          </button>
        )}
      </div>
    </>
  );
}

function NumberField({
  id,
  label,
  value,
  onChange,
}: {
  id: string;
  label: string;
  value: number;
  onChange: (value: number) => void;
}) {
  return (
    <div className="space-y-1.5">
      <Label htmlFor={id} className="text-xs">
        {label}
      </Label>
      <Input
        id={id}
        type="number"
        min={0}
        value={value}
        onChange={(e) => onChange(Number(e.target.value))}
      />
    </div>
  );
}
