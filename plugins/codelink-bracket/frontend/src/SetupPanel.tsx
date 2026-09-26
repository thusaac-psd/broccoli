import { useApiClient } from '@broccoli/web-sdk/api';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Button, Input, Label } from '@broccoli/web-sdk/ui';
import { cn } from '@broccoli/web-sdk/utils';
import { useQueryClient } from '@tanstack/react-query';
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

/**
 * Staff screen shown in place of the bracket until it is set up: choose and
 * seed the sixteen players, assign each round's problems, set the timing.
 * Creating the bracket also switches on this plugin's submission check for
 * the contest, which the round cannot run fairly without.
 */
export function SetupPanel({ contestId }: { contestId: number }) {
  const { t } = useTranslation();
  const api = useBracketApi();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();
  const participants = useParticipants(contestId, true);
  const { list: problemList, byId: problems } = useContestProblems(contestId);

  const [seeds, setSeeds] = useState<number[] | null>(null);
  const [rounds, setRounds] = useState<RoundDraft[] | null>(null);
  const [gameMinutes, setGameMinutes] = useState(30);
  const [breakMinutes, setBreakMinutes] = useState(10);
  const [graceSeconds, setGraceSeconds] = useState(120);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const people = participants.data ?? [];
  const nameOf = new Map(people.map((p) => [p.user_id, p.username]));
  // Until staff touch them, seeds follow registration order and problems
  // follow the contest's problem order.
  const seedList =
    seeds ??
    [...people]
      .sort((a, b) => a.registered_at.localeCompare(b.registered_at))
      .slice(0, PLAYER_COUNT)
      .map((p) => p.user_id);
  const roundList =
    rounds ??
    (problemList.length > 0
      ? autoFillRounds(problemList.map((p) => p.problem_id))
      : Array.from({ length: 4 }, emptyRound));
  const bench = people.filter((p) => !seedList.includes(p.user_id));
  const issues = validateSetup(seedList, roundList, gameMinutes);

  const setRound = (i: number, next: RoundDraft) =>
    setRounds(roundList.map((r, j) => (j === i ? next : r)));

  const create = async () => {
    setError(null);
    setSaving(true);
    try {
      await api.setup(contestId, {
        seeds: seedList,
        rounds: roundList.map((r) => ({
          group_a: r.groupA as [number, number, number],
          group_b: r.groupB as [number, number, number],
          tiebreak: r.tiebreak.filter((id): id is number => id !== null),
        })),
        xiaoju_seconds: Math.round(gameMinutes * 60),
        round_intermission_seconds: Math.round(breakMinutes * 60),
        escalation_grace_seconds: Math.round(graceSeconds),
      });
      const { error: gateError } = await apiClient.PUT(
        '/contests/{id}/config/{plugin_id}/{namespace}',
        {
          params: {
            path: {
              id: contestId,
              plugin_id: 'codelink-bracket',
              namespace: 'before_submission',
            },
          },
          body: { config: {}, enabled: true, position: 0 },
        },
      );
      if (gateError) throw new Error(t('codelink-bracket.setup.gateFailed'));
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

      <div className="grid gap-5 lg:grid-cols-[minmax(0,2fr)_minmax(0,3fr)]">
        <Panel
          title={t('codelink-bracket.setup.players', {
            count: seedList.length,
            total: PLAYER_COUNT,
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
                    disabled={seedList.length >= PLAYER_COUNT}
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
                      autoFillRounds(problemList.map((p) => p.problem_id)),
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
                <div key={i} className="rounded-lg border border-border p-3">
                  <div className="mb-2 text-xs font-semibold uppercase tracking-wide text-muted-foreground">
                    {t(roundNameKey(i + 1))}
                  </div>
                  <div className="grid gap-2 sm:grid-cols-[auto_1fr] sm:items-center">
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
                      onAdd={() =>
                        setRound(i, { ...r, tiebreak: [...r.tiebreak, null] })
                      }
                    />
                  </div>
                </div>
              ))}
            </div>
          </Panel>

          <Panel title={t('codelink-bracket.setup.timing')}>
            <div className="grid gap-3 sm:grid-cols-3">
              <NumberField
                id="bracket-game-minutes"
                label={t('codelink-bracket.setup.gameMinutes')}
                value={gameMinutes}
                onChange={setGameMinutes}
              />
              <NumberField
                id="bracket-break-minutes"
                label={t('codelink-bracket.setup.breakMinutes')}
                value={breakMinutes}
                onChange={setBreakMinutes}
              />
              <NumberField
                id="bracket-grace-seconds"
                label={t('codelink-bracket.setup.graceSeconds')}
                value={graceSeconds}
                onChange={setGraceSeconds}
              />
            </div>
          </Panel>
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
                <li key={i}>{describeIssue(issue, t, problems)}</li>
              ))}
            </ul>
          )}
          {error && <p className="mt-1 text-destructive">{error}</p>}
        </div>
        <ConfirmButton
          disabled={issues.length > 0 || saving}
          title={t('codelink-bracket.setup.confirmTitle')}
          description={t('codelink-bracket.setup.confirmDescription')}
          confirmLabel={t('codelink-bracket.setup.create')}
          onConfirm={() => void create()}
        >
          {saving
            ? t('codelink-bracket.setup.creating')
            : t('codelink-bracket.setup.create')}
        </ConfirmButton>
      </div>
    </div>
  );
}

function describeIssue(
  issue: SetupIssue,
  t: (key: string, params?: Record<string, string | number>) => string,
  problems: ReadonlyMap<number, { label: string }>,
): string {
  switch (issue.kind) {
    case 'players':
      return t('codelink-bracket.setup.issue.players', { count: issue.count });
    case 'emptySlot':
      return t('codelink-bracket.setup.issue.emptySlot', {
        round: t(roundNameKey(issue.round)),
      });
    case 'duplicate':
      return t('codelink-bracket.setup.issue.duplicate', {
        label: problems.get(issue.problemId)?.label ?? `#${issue.problemId}`,
      });
    case 'badTiming':
      return t('codelink-bracket.setup.issue.badTiming');
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
