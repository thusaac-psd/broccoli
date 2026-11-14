import { createContext } from 'react';

import type { MessageParams } from '@/i18n/message';

/**
 * An app types its own translation keys by augmenting this interface with a
 * `key` member, the union of the keys its catalog defines:
 *
 * ```ts
 * declare module '@broccoli/web-sdk/i18n' {
 *   interface TranslationRegister {
 *     key: keyof typeof en;
 *   }
 * }
 * ```
 *
 * `t` then rejects a key the catalog lacks at compile time. Without the
 * augmentation (plugins, whose keys come from their own packs) a key is any
 * string.
 */
// eslint-disable-next-line @typescript-eslint/no-empty-object-type -- filled by augmentation
export interface TranslationRegister {}

/** A translation key: the registered catalog's keys, else any string. */
export type TranslationKey = TranslationRegister extends { key: infer K }
  ? K
  : string;

export interface I18nContextValue {
  locale: string;
  setLocale: (locale: string) => void;
  availableLocales: string[];
  t: (key: TranslationKey, params?: MessageParams) => string;
  /**
   * The translation of a key only known at run time (a server error code, a
   * plugin-supplied identifier), or `undefined` when no catalog has it.
   */
  lookup: (key: string, params?: MessageParams) => string | undefined;
  isLoading: boolean;
}

export const I18nContext = createContext<I18nContextValue | null>(null);
