import type { en } from './en.ts';

/** A key of the host catalog. */
export type MessageKey = keyof typeof en;

/** A translation of the host catalog: every key, and no other. */
export type Catalog = Record<MessageKey, string>;

// `t()` in this app accepts only keys the host catalog defines, so a
// missing key is a compile error instead of raw text on screen.
declare module '@broccoli/web-sdk/i18n' {
  interface TranslationRegister {
    key: MessageKey;
  }
}
