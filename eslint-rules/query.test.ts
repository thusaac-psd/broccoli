import { test } from 'node:test';

import { RuleTester } from 'eslint';

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
        'const q = useQuery(o); const xs = q.data ?? []; const failed = q.isError;',
        'function useX() { const query = useQuery(o); const xs = query.data ?? []; return { xs, query }; }',
      ],
      invalid: [
        'const q = useQuery(o); const xs = q.data ?? [];',
        'const q = useInfiniteQuery(o); const xs = q.data || [];',
        'const q = useQuery(o); const m = useMemo(() => f(q.data ?? []), [q.data]);',
      ].map((code) => ({ code, errors: [{ messageId: 'defaulted' }] })),
    },
  );
});
