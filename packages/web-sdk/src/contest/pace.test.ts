import assert from 'node:assert/strict';
import { test } from 'node:test';

import { PLUGIN_OUTAGE_POLL_MS } from '../hooks/poll.ts';
import {
  contestPollPace,
  LIVE_BOARD_POLL_MS,
  liveBoardPace,
  nextPhaseChange,
  PHASE_DUE_POLL_MS,
  PHASE_DUE_SPREAD_MS,
  RUNNING_CONTEST_POLL_MS,
  type SeatPace,
  seatPace,
} from './pace.ts';

const times = {
  start_time: '2026-01-02T00:00:00Z',
  end_time: '2026-01-02T05:00:00Z',
};

test('the phase changes next at the start, then at the end', () => {
  assert.equal(
    nextPhaseChange({ phase: 'upcoming', ...times }),
    Date.parse(times.start_time),
  );
  assert.equal(
    nextPhaseChange({ phase: 'running', ...times }),
    Date.parse(times.end_time),
  );
  assert.equal(nextPhaseChange({ phase: 'ended', ...times }), null);
  assert.equal(nextPhaseChange({ phase: 'unactivated', ...times }), null);
});

/** A seat at `phaseOffsetMs` that asks about a running contest every minute. */
const seat = (phaseOffsetMs: number): SeatPace => ({
  phaseOffsetMs,
  runningPollMs: 60_000,
  liveBoardPollMs: LIVE_BOARD_POLL_MS,
});

const start = Date.parse(times.start_time);
const end = Date.parse(times.end_time);

test("a contest is asked for again once its phase is due, at this seat's offset", () => {
  const upcoming = { phase: 'upcoming', ...times } as const;
  assert.equal(contestPollPace(upcoming, start - 10_000, seat(700)), 10_700);
  assert.equal(contestPollPace(upcoming, start - 10, seat(700)), 710);
  const running = { phase: 'running', ...times } as const;
  assert.equal(contestPollPace(running, end - 5_000, seat(0)), 5_000);
});

test('a contest past its due phase change is asked at a steady pace, not reset by the clock', () => {
  const upcoming = { phase: 'upcoming', ...times } as const;
  // Every render asks again; the answer stays the same, so TanStack keeps
  // its timer instead of restarting it.
  assert.equal(
    contestPollPace(upcoming, start + 1, seat(0)),
    PHASE_DUE_POLL_MS,
  );
  assert.equal(
    contestPollPace(upcoming, start + 900, seat(0)),
    PHASE_DUE_POLL_MS,
  );
});

test('a contest whose plugin is out is asked whether it is back, slowly', () => {
  const ended = {
    phase: 'ended',
    ...times,
    plugin_outage: { state: 'disabled', plugin_id: 'ioi' },
  } as const;
  assert.equal(contestPollPace(ended, end + 1, seat(0)), PLUGIN_OUTAGE_POLL_MS);
  const running = { ...ended, phase: 'running' } as const;
  assert.equal(contestPollPace(running, end - 1_000, seat(0)), 1_000);
});

test('a contest with nothing due and no outage is not polled', () => {
  assert.equal(
    contestPollPace({ phase: 'ended', ...times }, end + 1, seat(0)),
    false,
  );
  assert.equal(
    contestPollPace({ phase: 'unactivated', ...times }, start - 1, seat(0)),
    false,
  );
});

test('a phase change too far off for a timer waits as long as a timer can', () => {
  const upcoming = { phase: 'upcoming', ...times } as const;
  assert.equal(
    contestPollPace(upcoming, start - 2 ** 40, seat(0)),
    2 ** 31 - 1,
  );
});

test('a running contest with nothing due is asked about slowly, so an outage that starts or ends shows', () => {
  const running = { phase: 'running', ...times } as const;
  assert.equal(contestPollPace(running, start + 1, seat(0)), 60_000);
  // Every render asks again; the answer stays put, so the timer runs out.
  assert.equal(contestPollPace(running, start + 30_000, seat(0)), 60_000);
  // The end, once nearer than the slow pace, is what it waits for.
  assert.equal(contestPollPace(running, end - 5_000, seat(0)), 5_000);
});

test('seats spread their asks about a running contest around the stated pace', () => {
  assert.deepEqual(
    seatPace(() => 0),
    {
      phaseOffsetMs: 0,
      runningPollMs: RUNNING_CONTEST_POLL_MS * 0.75,
      liveBoardPollMs: LIVE_BOARD_POLL_MS * 0.75,
    },
  );
  assert.deepEqual(
    seatPace(() => 1),
    {
      phaseOffsetMs: PHASE_DUE_SPREAD_MS,
      runningPollMs: RUNNING_CONTEST_POLL_MS * 1.25,
      liveBoardPollMs: LIVE_BOARD_POLL_MS * 1.25,
    },
  );
});

test('a live board asks at its seat pace, and stops with its contest once it is final', () => {
  const ended = {
    phase: 'ended' as const,
    start_time: new Date(0).toISOString(),
    end_time: new Date(1_000).toISOString(),
  };
  const slow = { ...seatPace(() => 1), phaseOffsetMs: 0 };
  assert.equal(
    liveBoardPace(ended, true, 10_000, slow),
    LIVE_BOARD_POLL_MS * 1.25,
  );
  assert.equal(
    liveBoardPace(undefined, true, 10_000, slow),
    LIVE_BOARD_POLL_MS * 1.25,
  );
  assert.equal(liveBoardPace(ended, false, 10_000, slow), false);
});
