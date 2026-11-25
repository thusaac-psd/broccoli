import type { I18nContextValue } from '@/i18n';

/** One option of a field whose value is chosen from what plugins registered. */
export interface RegistryChoice {
  value: string;
  /**
   * The stored value, which the registries loaded and do not list: its
   * plugin is disabled or not loaded on this server.
   */
  unavailable: boolean;
}

/**
 * The options of a field chosen from what the plugins registered (a problem
 * type, a contest type, a language): every registered value, and the stored
 * one when they do not list it, so a stored value always shows and saves as
 * it was. `registered` is undefined while the registries load or after they
 * failed: the stored value is then shown on its own, not as unavailable,
 * since nothing says it is.
 */
export function registryChoices(
  registered: readonly string[] | undefined,
  stored: string,
): RegistryChoice[] {
  const choices = (registered ?? []).map((value) => ({
    value,
    unavailable: false,
  }));
  if (stored && !registered?.includes(stored)) {
    choices.push({ value: stored, unavailable: registered !== undefined });
  }
  return choices;
}

/** What an option shows: its name, marked when its plugin is unavailable. */
export function registryChoiceLabel(
  choice: RegistryChoice,
  name: string,
  t: I18nContextValue['t'],
): string {
  return choice.unavailable
    ? t('admin.field.unavailableOption', { name })
    : name;
}
