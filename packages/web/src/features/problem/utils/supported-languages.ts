import type { Registries } from '@broccoli/web-sdk/hooks';

export interface SupportedLanguage {
  id: string;
  name: string;
  template: string;
  defaultFilename: string;
  extensions: string[];
}

/**
 * The languages the server can take work in now, from its registries
 * (`registriesQuery`): a language whose plugin is disabled is not among
 * them.
 */
export function supportedLanguages(
  registries: Pick<Registries, 'languages'>,
): SupportedLanguage[] {
  return (registries.languages ?? []).map((language) => ({
    id: language.id,
    name: language.name,
    defaultFilename: language.default_filename,
    template: language.template ?? '',
    extensions: language.extensions ?? [],
  }));
}
