import { useQuery } from '@tanstack/react-query';
import {
  type ReactNode,
  useCallback,
  useEffect,
  useMemo,
  useState,
} from 'react';

import { useApiClient } from '@/api';
import { I18nContext } from '@/i18n/i18n-context';
import { formatMessage, type MessageParams } from '@/i18n/message';
import type { TranslationMap } from '@/i18n/types';

/** localStorage key where the active locale is persisted. */
export const LOCALE_STORAGE_KEY = 'broccoli-locale';

interface I18nProviderProps {
  children: ReactNode;
  localeKey?: string;
  defaultLocale?: string;
  coreI18n?: Record<string, TranslationMap>;
}

export function I18nProvider({
  children,
  localeKey = LOCALE_STORAGE_KEY,
  defaultLocale = 'en',
  coreI18n = {},
}: I18nProviderProps) {
  const apiClient = useApiClient();

  const [locale, setLocaleState] = useState(defaultLocale);

  useEffect(() => {
    setLocaleState(() => {
      if (typeof window !== 'undefined') {
        const stored = localStorage.getItem(localeKey);
        // Prevent "undefined" string from being stored
        if (stored && stored !== 'undefined') {
          return stored;
        }
      }
      return defaultLocale;
    });
  }, [defaultLocale, localeKey]);

  const setLocale = useCallback(
    (newLocale: string) => {
      // Prevent invalid locales from being stored
      if (newLocale && newLocale !== 'undefined' && newLocale.length > 0) {
        setLocaleState(newLocale);
        if (typeof window !== 'undefined') {
          localStorage.setItem(localeKey, newLocale);
        }
      }
    },
    [localeKey],
  );

  // The bundled locales are offered before (and without) the server's list.
  const { data: availableLocales } = useQuery({
    queryKey: ['i18n', 'locales'],
    queryFn: async () => {
      const { data: pluginLocales = [] } = await apiClient.GET('/i18n/locales');
      const coreLocales = Object.keys(coreI18n);
      return Array.from(new Set([...coreLocales, ...pluginLocales]));
    },
  });

  const { data: pluginTranslations = {}, isLoading } = useQuery({
    queryKey: ['i18n', 'translations', locale],
    queryFn: async () => {
      const { data } = await apiClient.GET('/i18n/translations/{locale}', {
        params: { path: { locale } },
      });
      return data ?? {};
    },
    // Keep previous translations while loading new ones to prevent UI flickering
    placeholderData: (prev) => prev,
  });

  // Default-locale plugin translations, used as a fallback so plugin keys
  // missing from the active locale render the default-locale string instead
  // of the raw key. Shares the query key shape above, so the cache is reused
  // when the user switches back to the default locale.
  const { data: fallbackPluginTranslations = {} } = useQuery({
    queryKey: ['i18n', 'translations', defaultLocale],
    queryFn: async () => {
      const { data } = await apiClient.GET('/i18n/translations/{locale}', {
        params: { path: { locale: defaultLocale } },
      });
      return data ?? {};
    },
    enabled: locale !== defaultLocale,
    placeholderData: (prev) => prev,
  });

  const lookup = useCallback(
    (key: string, params?: MessageParams) => {
      const value =
        pluginTranslations[key] ??
        coreI18n[locale]?.[key] ??
        fallbackPluginTranslations[key] ??
        coreI18n[defaultLocale]?.[key];
      return value === undefined
        ? undefined
        : formatMessage(value, locale, params);
    },
    [
      locale,
      defaultLocale,
      pluginTranslations,
      fallbackPluginTranslations,
      coreI18n,
    ],
  );

  // A key no catalog has renders as itself, so a gap is visible.
  const t = useCallback(
    (key: string, params?: MessageParams) => lookup(key, params) ?? key,
    [lookup],
  );

  const value = useMemo(
    () => ({
      locale,
      setLocale,
      availableLocales: availableLocales ?? Object.keys(coreI18n),
      t,
      lookup,
      isLoading,
    }),
    [locale, setLocale, availableLocales, coreI18n, t, lookup, isLoading],
  );

  return <I18nContext value={value}>{children}</I18nContext>;
}
