// Message formatting for translated strings: a small subset of ICU
// MessageFormat that every catalog (host and plugin) uses.
//
// - `{name}` and `{{name}}` insert a parameter.
// - `{name, plural, =0 {…} one {…} other {…}}` picks a branch by the
//   locale's plural rules (`Intl.PluralRules`); an exact `=n` branch wins
//   over the category, and `other` is the fallback. Inside a branch `#` is
//   the number, formatted for the locale, and other placeholders work as
//   usual.
//
// A placeholder whose parameter is missing is left as written, so a missing
// parameter is visible instead of silently empty.

export type MessageParams = Record<string, string | number>;

const PLURAL_HEAD = /^\s*([A-Za-z0-9_]+)\s*,\s*plural\s*,/;
const SIMPLE = /^\s*([A-Za-z0-9_]+)\s*$/;
const BRANCH = /\s*(=\d+|zero|one|two|few|many|other)\s*\{/y;

const pluralRules = new Map<string, Intl.PluralRules>();
const numberFormats = new Map<string, Intl.NumberFormat>();

function cached<T>(map: Map<string, T>, locale: string, make: () => T): T {
  let value = map.get(locale);
  if (value === undefined) {
    value = make();
    map.set(locale, value);
  }
  return value;
}

/** The index just past the `}` that closes the `{` at `open`, or -1. */
function closingBrace(text: string, open: number): number {
  let depth = 0;
  for (let i = open; i < text.length; i += 1) {
    if (text[i] === '{') depth += 1;
    else if (text[i] === '}') {
      depth -= 1;
      if (depth === 0) return i + 1;
    }
  }
  return -1;
}

/** `{ selector: body }` for the branches of a plural argument. */
function pluralBranches(body: string): Map<string, string> | null {
  const branches = new Map<string, string>();
  let at = 0;
  while (at < body.length) {
    if (body.slice(at).trim() === '') break;
    BRANCH.lastIndex = at;
    const match = BRANCH.exec(body);
    if (!match) return null;
    const open = BRANCH.lastIndex - 1;
    const end = closingBrace(body, open);
    if (end < 0) return null;
    branches.set(match[1], body.slice(open + 1, end - 1));
    at = end;
  }
  return branches.has('other') ? branches : null;
}

function formatPlural(
  name: string,
  body: string,
  locale: string,
  params: MessageParams,
): string | null {
  const branches = pluralBranches(body);
  const raw = params[name];
  const count = typeof raw === 'number' ? raw : Number(raw);
  if (!branches || raw === undefined || Number.isNaN(count)) return null;
  const rules = cached(pluralRules, locale, () => new Intl.PluralRules(locale));
  const branch =
    branches.get(`=${count}`) ??
    branches.get(rules.select(count)) ??
    (branches.get('other') as string);
  const number = cached(numberFormats, locale, () =>
    Intl.NumberFormat(locale),
  ).format(count);
  return formatMessage(branch.replaceAll('#', number), locale, params);
}

/** Format `template` for `locale` with `params` (see the module docs). */
export function formatMessage(
  template: string,
  locale: string,
  params?: MessageParams,
): string {
  if (!params || !template.includes('{')) return template;
  let out = '';
  let at = 0;
  while (at < template.length) {
    const open = template.indexOf('{', at);
    if (open < 0) break;
    out += template.slice(at, open);
    const end = closingBrace(template, open);
    if (end < 0) {
      at = open;
      break;
    }
    const whole = template.slice(open, end);
    // `{{name}}` is the double-brace spelling of `{name}`.
    const inner =
      whole.startsWith('{{') && whole.endsWith('}}')
        ? whole.slice(2, -2)
        : whole.slice(1, -1);
    const simple = SIMPLE.exec(inner);
    const plural = PLURAL_HEAD.exec(inner);
    let replaced: string | null = null;
    if (simple && Object.hasOwn(params, simple[1])) {
      replaced = String(params[simple[1]]);
    } else if (plural) {
      replaced = formatPlural(
        plural[1],
        inner.slice(plural[0].length),
        locale,
        params,
      );
    }
    out += replaced ?? whole;
    at = end;
  }
  return out + template.slice(at);
}

/**
 * The parameter names `template` uses, sorted and without duplicates. Two
 * translations of one message must use the same names.
 */
export function messageParameters(template: string): string[] {
  const names = new Set<string>();
  const visit = (text: string) => {
    let at = 0;
    while (at < text.length) {
      const open = text.indexOf('{', at);
      if (open < 0) return;
      const end = closingBrace(text, open);
      if (end < 0) return;
      const whole = text.slice(open, end);
      const inner =
        whole.startsWith('{{') && whole.endsWith('}}')
          ? whole.slice(2, -2)
          : whole.slice(1, -1);
      const simple = SIMPLE.exec(inner);
      const plural = PLURAL_HEAD.exec(inner);
      if (simple) names.add(simple[1]);
      else if (plural) {
        names.add(plural[1]);
        for (const branch of pluralBranches(
          inner.slice(plural[0].length),
        )?.values() ?? []) {
          visit(branch);
        }
      }
      at = end;
    }
  };
  visit(template);
  return [...names].sort();
}
