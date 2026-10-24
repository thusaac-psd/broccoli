import type { MatchView } from '../types.ts';
import { gameLabel } from './stage.ts';

export interface GameNotification {
  key: string;
  label: ReturnType<typeof gameLabel>;
  result: 'won' | 'lost' | 'timeout';
  score: [number, number];
}

/** Compare settled games, including earlier rounds retained in the bracket. */
export function gameUpdates(
  matches: MatchView[],
  viewerId: number,
  seen: string[] | null,
): { notifications: GameNotification[]; seen: string[] } {
  const known = new Set(seen ?? []);
  const notifications: GameNotification[] = [];
  for (const match of matches
    .filter((m) => m.player_a === viewerId || m.player_b === viewerId)
    .sort((a, b) => a.round - b.round)) {
    let scoreA = 0;
    let scoreB = 0;
    for (const game of match.games) {
      if (!game.decided) continue;
      scoreA += Number(game.winner === match.player_a);
      scoreB += Number(game.winner === match.player_b);
      const key = `game:${match.id}:${game.index}:${game.opened_at_ms}`;
      // The first snapshot establishes a baseline, not a batch of old alerts.
      if (seen !== null && !known.has(key)) {
        notifications.push({
          key,
          label: gameLabel(match, game.index),
          result:
            game.winner === null
              ? 'timeout'
              : game.winner === viewerId
                ? 'won'
                : 'lost',
          score:
            viewerId === match.player_a ? [scoreA, scoreB] : [scoreB, scoreA],
        });
      }
      known.add(key);
    }
  }
  return { notifications, seen: [...known] };
}

export function gameNotificationText(
  notification: GameNotification,
  t: (key: string, params?: Record<string, string | number>) => string,
): { title: string; body: string } {
  return {
    title: t('codelink-bracket.notifications.title', {
      game: t(...notification.label),
    }),
    body: t(`codelink-bracket.notifications.${notification.result}`, {
      own: notification.score[0],
      opponent: notification.score[1],
    }),
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
