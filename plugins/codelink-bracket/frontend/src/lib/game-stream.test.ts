import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { GameView, MatchView } from '../types.ts';
import { makeMatch } from './fixtures.test-util.ts';
import { RECONNECT_MS, runGameStream } from './game-stream.ts';

function game(index: number, winner: number | null, decided = true): GameView {
  return {
    index,
    winner,
    decided,
    tiebreak: false,
    opened_at_ms: 1000 + index,
    deadline_ms: 2000 + index,
    problem_a: 101,
    problem_b: 201,
  };
}

/** A stream that sends one snapshot of `matches` and ends. */
function snapshotStream(matches: MatchView[]): Response {
  const text = `event: snapshot\ndata: ${JSON.stringify({ matches })}\n\n`;
  return new Response(text, {
    headers: { 'content-type': 'text/event-stream' },
  });
}

/** One attempt to open the stream: what it answers and how long it lives. */
type Attempt = { answer: Response | Error; livesMs: number };

/**
 * Runs the stream of viewer 10 through `attempts`, on a clock the attempts
 * alone move. Once they run out the viewer leaves the page.
 */
async function follow(attempts: Attempt[], saved: string[] | null = null) {
  const controller = new AbortController();
  let clock = 0;
  const notified: string[] = [];
  const waits: { failures: number; wakesOnline: boolean }[] = [];
  const connected: boolean[] = [];
  let seen = saved;
  let opened = 0;
  let failures = 0;
  await runGameStream({
    open: async () => {
      const attempt = attempts[opened++];
      if (!attempt) {
        controller.abort();
        throw new DOMException('left the page', 'AbortError');
      }
      clock += attempt.livesMs;
      if (attempt.answer instanceof Error) throw attempt.answer;
      return attempt.answer;
    },
    now: () => clock,
    retryDelay: (count) => {
      failures = count;
      return count * RECONNECT_MS;
    },
    wait: async (_ms, wakesOnline) => {
      waits.push({ failures, wakesOnline });
    },
    connected: (on) => connected.push(on),
    snapshot: () => {},
    notify: (notification) => {
      notified.push(notification.key);
    },
    seen: {
      read: () => seen,
      write: (next) => {
        seen = next;
      },
    },
    viewerId: 10,
    signal: controller.signal,
  });
  return { notified, waits, connected, opened };
}

const healthy = RECONNECT_MS * 30;

test('a game decided while the stream was down is notified once, from the snapshot after it reconnects', async () => {
  const pending = [makeMatch({ games: [game(0, null, false)] })];
  const decided = [makeMatch({ games: [game(0, 10)] })];
  const run = await follow([
    { answer: snapshotStream(pending), livesMs: healthy },
    { answer: new TypeError('fetch failed'), livesMs: 0 },
    { answer: snapshotStream(decided), livesMs: healthy },
    { answer: snapshotStream(decided), livesMs: healthy },
  ]);
  assert.deepEqual(run.notified, ['game:0:0:1000']);
  assert.deepEqual(run.connected, [
    true,
    false,
    false,
    true,
    false,
    true,
    false,
  ]);
});

test('a stream the server closes after a healthy life is opened again at once', async () => {
  const matches = [makeMatch()];
  const run = await follow([
    { answer: snapshotStream(matches), livesMs: healthy },
    { answer: snapshotStream(matches), livesMs: healthy },
  ]);
  assert.deepEqual(run.waits, []);
  assert.equal(run.opened, 3);
});

test('failed attempts back off longer each time, and a healthy stream starts the count again', async () => {
  const matches = [makeMatch()];
  const unavailable = new Response(null, { status: 503 });
  const run = await follow([
    { answer: new TypeError('fetch failed'), livesMs: 0 },
    { answer: unavailable, livesMs: 0 },
    { answer: snapshotStream(matches), livesMs: healthy },
    // A stream that ends before `RECONNECT_MS` counts as a failure.
    { answer: snapshotStream(matches), livesMs: 0 },
  ]);
  assert.deepEqual(run.waits, [
    // Only a failure of the network ends its wait when the browser is online.
    { failures: 1, wakesOnline: true },
    { failures: 2, wakesOnline: false },
    { failures: 1, wakesOnline: false },
  ]);
});

test('a viewer the server refuses the bracket stops following it', async () => {
  const run = await follow([
    { answer: new Response(null, { status: 403 }), livesMs: 0 },
    { answer: snapshotStream([makeMatch()]), livesMs: healthy },
  ]);
  assert.equal(run.opened, 1);
  assert.deepEqual(run.waits, []);
  assert.ok(!run.connected.includes(true), 'it never connected');
});

test('the games seen before a reload are the baseline, so only games decided since are notified', async () => {
  const decided = [makeMatch({ games: [game(0, 10), game(1, 20)] })];
  const reloaded = await follow(
    [{ answer: snapshotStream(decided), livesMs: healthy }],
    ['game:0:0:1000'],
  );
  assert.deepEqual(reloaded.notified, ['game:0:1:1001']);

  // With no games seen yet, the first snapshot is the baseline.
  const fresh = await follow([
    { answer: snapshotStream(decided), livesMs: healthy },
  ]);
  assert.deepEqual(fresh.notified, []);
});
