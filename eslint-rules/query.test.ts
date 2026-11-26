import { join } from 'node:path';
import { test } from 'node:test';

import { RuleTester } from 'eslint';
import tseslint from 'typescript-eslint';

import query from './query.js';

const tester = new RuleTester({
  languageOptions: {
    parserOptions: { ecmaFeatures: { jsx: true } },
  },
});

test('a query read with a fallback renders its state', () => {
  tester.run(
    'no-defaulted-query-data',
    query.rules['no-defaulted-query-data'],
    {
      valid: [
        'const q = useQuery(o); const s = useQueryState(q); const xs = q.data ?? [];',
        'const q = useQuery(o); const s = useQueryState(a ? q : p); const xs = q.data ?? [];',
        'const q = useQuery(o); const xs = q.data ?? []; const v = <QueryView query={q} />;',
        'const q = useQuery(o); const xs = q.data;',
        'const r = await apiClient.GET(p); const xs = r.data ?? [];',
        'const q = useQuery(o); const n = q.data?.length ?? 0; const s = useQueryState(q);',
        'const q = useQuery(o); const xs = q.data ?? []; const failed = q.isError; f(failed);',
        'const q = useQuery(o); const xs = q.data ?? []; const v = q.isError ? <Failed /> : <List xs={xs} />;',
        'function useX() { const query = useQuery(o); const xs = query.data ?? []; return { xs, query }; }',
        'const { data: rows, isError } = useQuery(o); const xs = rows ?? []; f(isError);',
        'const { data, error } = useQuery(o); const xs = data ?? []; const v = <E error={error} />;',
        'const { data: rows } = useQuery(o); const n = rows?.length;',
        'const q = useQuery(o); const s = useQueryState(q); const { data } = q; const xs = data ?? [];',
        'const { data: rows, error } = useQuery(o); const xs = rows?.items ?? []; if (error) throw error;',
        'const { isError, ...rest } = useQuery(o); const xs = rest.data ?? [];',
        'const xs = useQuery(o).data;',
        'const q = useQuery(o); const n = q.data?.max ?? undefined;',
        'const q = useQuery(o); const n = q.data ?? null;',
        'const q = useQuery(o); const rows = q.data; const xs = rows ?? []; const s = useQueryState(q);',
        'const q = useQuery(o); const xs = q.data ? q.data : []; if (q.isError) f();',
        'function C() { const q = useQuery(o); if (!q.data) return null; return q.data.length; }',
        'function C() { const q = useQuery(o); if (!q.data) return; g(q.data); }',
        'function C() { const q = useQuery(o); const s = useQueryState(q); if (!q.data) return EMPTY; return q.data; }',
        'const q = useQuery(o); const n = q.data ? 1 : 0; const s = useQueryState(q);',
      ],
      invalid: [
        'const q = useQuery(o); const xs = q.data ?? [];',
        'const q = useInfiniteQuery(o); const xs = q.data || [];',
        'const q = useQuery(o); const m = useMemo(() => f(q.data ?? []), [q.data]);',
        'const { data: rows } = useQuery(o); const xs = rows ?? [];',
        'const { data } = useQuery(o); const xs = data || [];',
        'const { data: rows = [] } = useQuery(o);',
        'const xs = useQuery(o).data ?? [];',
        'const q = useQuery(o); const { data } = q; const xs = data ?? [];',
        'const { data: rows } = useQuery(o); const xs = rows?.items ?? [];',
        'const { isLoading, ...rest } = useQuery(o); const xs = rest.data ?? [];',
        'const q = useQuery(o); const xs = q.data?.items ?? [];',
        'const q = useQuery(o); const xs = q.data ? q.data.map(f) : [];',
        'const q = useQuery(o); const xs = !q.data ? [] : q.data;',
        'const q = useQuery(o); const xs = q.data === undefined ? [] : q.data;',
        'const q = useQuery(o); const rows = q.data; const xs = rows ?? [];',
        'const rows = useQuery(o).data; const xs = rows ?? [];',
        'function C() { const q = useQuery(o); if (!q.data) return EMPTY; return q.data; }',
        'function C() { const q = useQuery(o); if (q.data === undefined) { return []; } return q.data; }',
        'const q = useQuery(o); const xs = Array.isArray(q.data) ? q.data : EMPTY;',
        'function C() { const q = useQuery(o); if (!Array.isArray(q.data)) return EMPTY; return q.data; }',
        'const q = useQuery(o); const xs = (q.data ?? null) ?? [];',
        'const q = useQuery(o); const rows = q.data ?? null; const xs = rows ?? [];',
        'const q = useQuery(o); const xs = q.data ?? []; void q.isError;',
        'const q = useQuery(o); const xs = q.data ?? []; q.isError;',
        'const q = useQuery(o); const xs = q.data ?? []; const failed = q.isError;',
        'const { data: rows, isError } = useQuery(o); const xs = rows ?? [];',
      ].map((code) => ({ code, errors: [{ messageId: 'defaulted' }] })),
    },
  );
});

// With types, a query result is whatever has one's shape, whoever made it.
const PRELUDE = `
type Result<T> = {
  data: T | undefined;
  isError: boolean;
  error: unknown;
  fetchStatus: 'idle' | 'fetching' | 'paused';
  refetch: () => void;
};
type State<T> = { status: 'data'; data: T } | { status: 'loading' };
declare function useQuery(o: unknown): Result<string[]>;
declare function useRegistries(): Result<{ languages: string[] }>;
declare function useQueries(o: unknown): Result<string[]>[];
declare function useQueryState<T>(q: Result<T>): State<T>;
declare function useView(): State<string[]>;
declare const apiClient: { GET(p: string): Promise<{ data?: string[]; error?: unknown }> };
declare const a: boolean;
declare function f(x: unknown): string[];
`;

test('with types, any query result read with a fallback renders its state', () => {
  const typed = new RuleTester({
    languageOptions: {
      parser: tseslint.parser,
      parserOptions: {
        projectService: {
          allowDefaultProject: ['eslint-rules/*.tsx'],
          defaultProject: 'tsconfig.json',
        },
        tsconfigRootDir: join(import.meta.dirname, '..'),
      },
    },
  });
  const cases = (codes: string[]) =>
    codes.map((code) => ({
      code: PRELUDE + code,
      filename: join(import.meta.dirname, 'typed.tsx'),
    }));
  typed.run('no-defaulted-query-data', query.rules['no-defaulted-query-data'], {
    valid: cases([
      'const q = useRegistries(); const s = useQueryState(q); const xs = q.data?.languages ?? [];',
      'const q = useRegistries(); const xs = q.data?.languages; ',
      'const v = useView(); const xs = v.status === "data" ? v.data : [];',
      'async function g() { const r = await apiClient.GET("p"); return r.data ?? []; }',
      'const qs = useQueries(o); const s = useQueryState(qs[0]); const xs = qs[0].data ?? [];',
      'function useX() { const query = useRegistries(); return query; }',
      'const { data, isError } = useRegistries(); const xs = data?.languages ?? []; if (isError) f(0);',
      'function V({ query }: { query: Result<string[]> }) { const s = useQueryState(query); return query.data ?? []; }',
      'const q = useRegistries(); const xs = q.data && (q.data.languages[0] ?? "none");',
      'const q = useRegistries(); if (q.data) { const xs = q.data.languages ?? []; }',
    ]),
    invalid: cases([
      'const q = useRegistries(); const xs = q.data?.languages ?? [];',
      'const { data: registries } = useRegistries(); const xs = registries?.languages ?? [];',
      'const xs = useRegistries().data?.languages ?? [];',
      'const q = useRegistries(); const p = q; const xs = p.data ?? [];',
      'const q = a ? useQuery(1) : useQuery(2); const xs = q.data ?? [];',
      'const qs = useQueries(o); const xs = qs[0].data ?? [];',
      'const [first] = useQueries(o); const xs = first.data ?? [];',
      'const qs = useQueries(o); const xs = qs.map((q) => q.data ?? []);',
      'const [{ data }] = useQueries(o); const xs = data ?? [];',
      'function V({ query }: { query: Result<string[]> }) { return query.data ?? []; }',
      'const q = useRegistries(); const rows = q.data; const xs = rows ?? f(0);',
      'const q = useRegistries(); const xs = q.data ? q.data.languages : [];',
    ]).map((c) => ({ ...c, errors: [{ messageId: 'defaulted' }] })),
  });
});
