import type { sourceCatalog } from './packs.gen.ts';

/** A key of the host's messages: the keys of the English language pack. */
export type MessageKey = keyof typeof sourceCatalog;

// `t()` in this app accepts only keys the English pack defines, so a missing
// or misspelled key is a compile error instead of raw text on screen.
declare module '@broccoli/web-sdk/i18n' {
  interface TranslationRegister {
    key: MessageKey;
  }
}
