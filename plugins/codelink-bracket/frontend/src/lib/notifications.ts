export interface GameEnded {
  match_id: number;
  round: number;
  game_index: number;
  game_number: number;
  tiebreak: boolean;
  winner: number | null;
  player_a: number;
  player_b: number;
  score_a: number;
  score_b: number;
}

export interface GameNotification {
  id: string;
  key: string;
  game: GameEnded;
}

export function isCursor(value: unknown): value is string {
  return typeof value === 'string' && /^(0|[1-9]\d*)$/.test(value);
}

export function parseGameNotification(
  data: string,
  viewerId: number,
): GameNotification | null {
  let wire: { id?: unknown; key?: unknown; kind?: unknown; payload?: unknown };
  try {
    wire = JSON.parse(data);
  } catch {
    return null;
  }
  if (
    !wire ||
    !isCursor(wire.id) ||
    typeof wire.key !== 'string' ||
    !wire.key ||
    wire.kind !== 'game-ended'
  )
    return null;
  const g = wire.payload as GameEnded | undefined;
  if (
    !g ||
    ![
      g.match_id,
      g.round,
      g.game_index,
      g.game_number,
      g.player_a,
      g.player_b,
      g.score_a,
      g.score_b,
    ].every((n) => Number.isSafeInteger(n) && n >= 0) ||
    g.round < 1 ||
    g.game_number < 1 ||
    typeof g.tiebreak !== 'boolean' ||
    (g.player_a !== viewerId && g.player_b !== viewerId) ||
    (g.winner !== null && g.winner !== g.player_a && g.winner !== g.player_b)
  ) {
    return null;
  }
  return { id: wire.id, key: wire.key, game: g };
}

export function gameNotificationText(
  game: GameEnded,
  viewerId: number,
  t: (key: string, params?: Record<string, string | number>) => string,
): { title: string; body: string } {
  const label = t(
    `codelink-bracket.game.${game.tiebreak ? 'tiebreak' : 'regular'}`,
    { n: game.game_number },
  );
  const result =
    game.winner === null
      ? 'timeout'
      : game.winner === viewerId
        ? 'won'
        : 'lost';
  const ownScore = viewerId === game.player_a ? game.score_a : game.score_b;
  const theirScore = viewerId === game.player_a ? game.score_b : game.score_a;
  return {
    title: t('codelink-bracket.notifications.title', { game: label }),
    body: `${t(`codelink-bracket.notifications.${result}`)} ${t('codelink-bracket.notifications.score', { own: ownScore, opponent: theirScore })}`,
  };
}

/** Atomic IndexedDB add arbitrates simultaneous deliveries in different tabs. */
export function claimDesktopNotification(key: string): Promise<boolean> {
  return new Promise((resolve, reject) => {
    let settled = false;
    const request = indexedDB.open('broccoli-codelink-notifications', 1);
    request.onupgradeneeded = () => {
      request.result.createObjectStore('delivered');
    };
    const fail = (error: unknown) => {
      settled = true;
      reject(error);
    };
    request.onerror = () => fail(request.error);
    request.onblocked = () => fail(new Error('Notification storage blocked'));
    request.onsuccess = () => {
      const db = request.result;
      if (settled) {
        db.close();
        return;
      }
      db.onversionchange = () => db.close();
      try {
        const tx = db.transaction('delivered', 'readwrite');
        const add = tx.objectStore('delivered').add(Date.now(), key);
        let duplicate = false;
        add.onerror = () => {
          duplicate = add.error?.name === 'ConstraintError';
        };
        tx.oncomplete = () => {
          db.close();
          settled = true;
          resolve(true);
        };
        tx.onabort = () => {
          db.close();
          settled = true;
          if (duplicate) resolve(false);
          else reject(tx.error);
        };
      } catch (error) {
        db.close();
        fail(error);
      }
    };
  });
}

/** Storage may be disabled by the browser; page notifications still work. */
export function readStorage(
  storage: 'localStorage' | 'sessionStorage',
  key: string,
): string | null {
  try {
    return window[storage].getItem(key);
  } catch {
    return null;
  }
}

export function writeStorage(
  storage: 'localStorage' | 'sessionStorage',
  key: string,
  value: string,
): void {
  try {
    window[storage].setItem(key, value);
  } catch {
    // In-memory state remains usable when persistence is unavailable.
  }
}
