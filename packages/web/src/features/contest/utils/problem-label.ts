const LETTERS = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ';

/**
 * The first contest problem label not in `used`: A to Z, then AA to ZZ.
 * Empty when all 702 are taken.
 */
export function nextProblemLabel(used: ReadonlySet<string>): string {
  for (const first of LETTERS) {
    if (!used.has(first)) return first;
  }
  for (const first of LETTERS) {
    for (const second of LETTERS) {
      if (!used.has(first + second)) return first + second;
    }
  }
  return '';
}
