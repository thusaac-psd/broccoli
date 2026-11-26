import { createContext } from 'react';

/**
 * The config as the form last read or saved it, with each keyed item marked
 * by the key it was saved under (`markSaved`): what a field reads its stored
 * secret from, by its item's identity rather than its row.
 */
export const StoredValues = createContext<Record<string, unknown>>({});
