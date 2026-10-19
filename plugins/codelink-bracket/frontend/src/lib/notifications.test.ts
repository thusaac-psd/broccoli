import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  gameNotificationText,
  isCursor,
  parseGameNotification,
} from './notifications.ts';

const game = {
  match_id: 0,
  round: 1,
  game_index: 1,
  game_number: 2,
  tiebreak: false,
  winner: 10,
  player_a: 10,
  player_b: 20,
  score_a: 1,
  score_b: 0,
};
const wire = (payload: unknown, kind = 'game-ended') =>
  JSON.stringify({
    id: '9007199254740993',
    key: 'game:0:1:123',
    kind,
    payload,
  });

test('small-game messages apply to both players and exclude spectators', () => {
  assert.equal(parseGameNotification(wire(game), 10)?.game.game_index, 1);
  assert.equal(parseGameNotification(wire(game), 20)?.game.game_index, 1);
  assert.equal(parseGameNotification(wire(game), 30), null);
  assert.equal(parseGameNotification(wire(game, 'match-ended'), 10), null);
});

test('untrusted or malformed events cannot become game notifications', () => {
  for (const data of [
    'null',
    '{',
    wire(null),
    wire({ ...game, winner: 30 }),
    wire({ ...game, game_number: -1 }),
  ]) {
    assert.equal(parseGameNotification(data, 10), null);
  }
  assert.equal(isCursor('9007199254740993'), true);
  for (const cursor of [-1, 1, '-1', '1e3', '01', ''])
    assert.equal(isCursor(cursor), false);
});

test('wording distinguishes win, loss, timeout and numbered tiebreaks', () => {
  const t = (key: string, params?: Record<string, string | number>) =>
    `${key}:${JSON.stringify(params)}`;
  assert.match(gameNotificationText(game, 10, t).body, /notifications.won/);
  assert.match(gameNotificationText(game, 20, t).body, /notifications.lost/);
  assert.match(gameNotificationText(game, 20, t).body, /"own":0,"opponent":1/);
  assert.match(
    gameNotificationText({ ...game, winner: null }, 10, t).body,
    /notifications.timeout/,
  );
  const { title } = gameNotificationText(
    { ...game, tiebreak: true, game_number: 1 },
    10,
    t,
  );
  assert.equal(
    JSON.parse(title.slice(title.indexOf(':') + 1)).game,
    'codelink-bracket.game.tiebreak:{"n":1}',
  );
});
