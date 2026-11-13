import { use } from 'react';

import { I18nContext } from '@/i18n/i18n-context';

export function useTranslation() {
  const context = use(I18nContext);
  if (!context) {
    throw new Error('useTranslation must be used within an I18nProvider');
  }
  return context;
}
