import js from '@eslint/js';
import { defineConfig, globalIgnores } from 'eslint/config';
import eslintConfigPrettier from 'eslint-config-prettier/flat';
import i18next from 'eslint-plugin-i18next';
import importPlugin from 'eslint-plugin-import';
import simpleImportSort from 'eslint-plugin-simple-import-sort';
import tsdoc from 'eslint-plugin-tsdoc';
import tseslint from 'typescript-eslint';
import reactDom from 'eslint-plugin-react-dom';
import reactHooks from 'eslint-plugin-react-hooks';
import reactRefresh from 'eslint-plugin-react-refresh';
import reactX from 'eslint-plugin-react-x';
import globals from 'globals';

import a11y from './eslint-rules/a11y.js';
import query from './eslint-rules/query.js';

export default defineConfig([
  // Base config for all files
  globalIgnores([
    '**/dist/*',
    '**/coverage/*',
    'packages/web-sdk/src/api/schema.ts',
    'packages/web/.react-router/**',
    'packages/web/src/lib/i18n/packs.gen.ts',
    // The docs site is not a member of the root pnpm workspace; its
    // dependencies install from website/pnpm-lock.yaml.
    'website/**',
  ]),

  // TypeScript files
  {
    extends: [
      js.configs.recommended,
      ...tseslint.configs.recommended,
      eslintConfigPrettier,
    ],
    files: ['**/*.ts', '**/*.tsx'],
    languageOptions: {
      parser: tseslint.parser,
      parserOptions: {
        projectService: true, // respects `tsconfig.json` in each package
      },
    },
    plugins: {
      '@typescript-eslint': tseslint.plugin,
      'simple-import-sort': simpleImportSort,
      import: importPlugin,
      tsdoc: tsdoc,
    },
    rules: {
      '@typescript-eslint/no-unused-vars': [
        'error',
        { argsIgnorePattern: '^_', varsIgnorePattern: '^_' },
      ],
      'simple-import-sort/imports': 'warn',
      'simple-import-sort/exports': 'warn',
      'import/newline-after-import': 'warn',
      'import/no-duplicates': 'warn',
      'tsdoc/syntax': 'warn',
    },
  },

  // Client clocks. A contestant PC's clock can be hours off the server's, so
  // anything compared with a server timestamp reads the server clock.
  {
    files: [
      'packages/web/src/**/*.{ts,tsx}',
      'packages/web-sdk/src/**/*.{ts,tsx}',
      'plugins/*/{frontend,web}/src/**/*.{ts,tsx}',
    ],
    ignores: ['**/*.test.ts', 'packages/web-sdk/src/api/server-clock.ts'],
    rules: {
      'no-restricted-properties': [
        'error',
        {
          object: 'Date',
          property: 'now',
          message:
            "Date.now() reads this machine's clock, which can be far off the server's. Compare server timestamps with serverNow(), useServerNow() or useServerTimeReached() from @broccoli/web-sdk/api, and measure elapsed time with performance.now().",
        },
      ],
    },
  },

  // Heavy code loads on the page that shows it. Every route module, and all
  // it imports statically, loads for every visitor once the app has started
  // (see `lib/preload-routes.ts`), so Monaco and KaTeX are imported at run
  // time only by `*.impl` modules and their setup modules, which code reaches
  // through a dynamic `import()` (see `components/lazy-impl.ts`).
  {
    files: ['packages/web/src/**/*.{ts,tsx}'],
    ignores: [
      '**/*.impl.{ts,tsx}',
      'packages/web/src/lib/monaco-setup.ts',
      'packages/web/src/components/markdown-math.ts',
    ],
    rules: {
      '@typescript-eslint/no-restricted-imports': [
        'error',
        {
          patterns: [
            {
              group: [
                'monaco-editor',
                'monaco-editor/*',
                '@monaco-editor/react',
                'monaco-vim',
                'katex',
                'katex/*',
                'rehype-katex',
                'remark-math',
                '**/monaco-setup',
                '**/markdown-math',
                '**/*.impl',
              ],
              allowTypeImports: true,
              message:
                'Heavy code loads through a dynamic import (lazyImpl), never with a route: import it statically only from an *.impl module.',
            },
          ],
        },
      ],
    },
  },

  // Frontend-specific React config
  {
    files: ['packages/web/**/*.{ts,tsx}', 'packages/web-sdk/**/*.tsx'],
    languageOptions: {
      ecmaVersion: 2020,
      globals: globals.browser,
    },
    plugins: {
      'react-hooks': reactHooks,
      'react-refresh': reactRefresh,
      'react-x': reactX,
      'react-dom': reactDom,
    },
    rules: {
      ...reactHooks.configs.recommended.rules,
      ...reactX.configs['recommended-typescript'].rules,
      ...reactDom.configs.recommended.rules,
      'react-refresh/only-export-components': [
        'warn',
        // `handle` is a React Router route module export.
        { allowConstantExport: true, allowExportNames: ['handle'] },
      ],
    },
  },

  // Every control has an accessible name.
  {
    files: [
      'packages/web/src/**/*.tsx',
      'packages/web-sdk/src/**/*.tsx',
      'plugins/*/frontend/src/**/*.tsx',
    ],
    plugins: { a11y },
    rules: {
      'a11y/icon-button-label': 'error',
      'a11y/img-alt': 'error',
      'a11y/input-label': 'error',
      'a11y/select-label': 'error',
      'a11y/text-token-opacity': 'error',
    },
  },

  // A query's data is read with a fallback only where its state is shown.
  {
    files: [
      'packages/web/src/**/*.{ts,tsx}',
      'packages/web-sdk/src/**/*.{ts,tsx}',
      'plugins/*/frontend/src/**/*.{ts,tsx}',
    ],
    plugins: { query },
    rules: {
      'query/no-defaulted-query-data': 'error',
    },
  },

  // User-facing text comes from translation keys, never from literals: JSX
  // text children, string expressions rendered in JSX, and the attributes a
  // user reads or hears. Class names, identifiers and symbols are exempt.
  {
    files: [
      'packages/web/src/**/*.tsx',
      'packages/web-sdk/src/**/*.tsx',
      'plugins/*/frontend/src/**/*.tsx',
      'plugins/*/web/src/**/*.tsx',
    ],
    plugins: { i18next },
    rules: {
      'i18next/no-literal-string': [
        'error',
        {
          mode: 'jsx-only',
          'should-validate-template': true,
          'jsx-attributes': {
            include: [
              'placeholder',
              'title',
              'alt',
              'label',
              'description',
              'tooltip',
              'hint',
              'aria-(label|description|placeholder|roledescription|valuetext)',
              '[a-z][A-Za-z]*(Label|Title|Placeholder|Message|Description|Hint)',
            ],
          },
          words: {
            exclude: [
              // Numbers, punctuation and symbols.
              '[0-9!-/:-@[-`{-~\\s·—–→←×✓…▾▸]+',
              // Colors.
              '#[0-9a-fA-F]{3,8}',
              'var\\(--[a-z-]+\\)',
              // A single class name or kebab-case identifier.
              '[a-z0-9:./\\[\\]()!%#_-]*-[a-z0-9:./\\[\\]()!%#_-]*',
              // snake_case identifiers.
              '[a-z0-9]+(_[a-z0-9]+)+',
            ],
          },
          callees: {
            exclude: [
              't',
              'cn',
              'clsx',
              'format',
              'require',
              'includes',
              'startsWith',
              'endsWith',
              'join',
              'split',
            ],
          },
        },
      ],
    },
  },
  // Outside JSX: messages shown through toasts and error state, and the
  // label/title/message fields handed to UI components, in .ts files too.
  // Class names and dotted translation keys in those fields are exempt.
  {
    files: [
      'packages/web/src/**/*.{ts,tsx}',
      'packages/web-sdk/src/**/*.{ts,tsx}',
      'plugins/*/frontend/src/**/*.{ts,tsx}',
      'plugins/*/web/src/**/*.{ts,tsx}',
    ],
    ignores: ['**/*.test.ts', '**/*.test-util.ts'],
    rules: {
      'no-restricted-syntax': [
        'error',
        {
          selector:
            "CallExpression[callee.object.name='toast'] > :matches(Literal[value=/[A-Za-z]/], TemplateLiteral)",
          message: 'Toast text comes from a translation key: use t().',
        },
        {
          // The JSX rule lets DOM elements' aria attributes through.
          selector:
            'JSXAttribute[name.name=/^aria-(label|description|placeholder|roledescription|valuetext)$/] > Literal[value=/[A-Za-z]/]',
          message: 'Accessible text comes from a translation key: use t().',
        },
        {
          selector:
            'CallExpression[callee.name=/^set(Error|Message|Notice)$/] > :matches(Literal[value=/[A-Za-z]/], TemplateLiteral)',
          message: 'Shown error text comes from a translation key: use t().',
        },
        {
          // `const { data = [] } = useQuery(...)` shows a failed or unfinished
          // fetch as an empty list. Render the query through QueryView or
          // useQueryState() from @broccoli/web-sdk, which keep the states apart.
          selector:
            'VariableDeclarator[init.type="CallExpression"][init.callee.name=/^use[A-Z]/] > ObjectPattern > Property[key.name="data"] > AssignmentPattern',
          message:
            'Do not default a query result: render it through QueryView or useQueryState() so loading, failure and empty stay distinct.',
        },
        {
          // `const { data, isLoading } = useQuery(...)` has no failed state:
          // a list that could not load reads as empty once loading ends.
          selector:
            'VariableDeclarator[init.type="CallExpression"][init.callee.name="useQuery"] > ObjectPattern:has(> Property[key.name="data"]):has(> Property[key.name=/^is(Loading|Pending)$/]):not(:has(> Property[key.name=/^(error|isError|status)$/]))',
          message:
            'A query read without its failure shows a failed fetch as empty: render it through QueryView or useQueryState().',
        },
        {
          // A poll read field by field without its failure never says that
          // its refreshes fail: the last good copy ages on screen and reads
          // as current.
          selector:
            'VariableDeclarator[init.callee.name="useQuery"]:has(ObjectExpression > Property[key.name="refetchInterval"]) > ObjectPattern:not(:has(> Property[key.name=/^(error|isError|status)$/]))',
          message:
            'A polling query read without its failure hides that its refreshes fail: render it through QueryView, or useQueryState() and RefreshFailedNotice.',
        },
        {
          // A fixed `refetchInterval` asks a failing server as often as a
          // healthy one, from every seat at once, and flickers a page that
          // has nothing to show between loading and failed.
          selector:
            "ObjectExpression > Property[key.name='refetchInterval']:not([value.type='CallExpression'][value.callee.name='pollInterval'])",
          message:
            'Poll through pollInterval() from @broccoli/web-sdk/hooks, which backs off while polls fail.',
        },
        {
          // A timer that calls refetch() polls beside the query: every
          // component that mounts it starts its own, each refetch cancels
          // the one still in flight, and nothing backs off or spreads seats.
          selector:
            "CallExpression[callee.name=/^set(Interval|Timeout)$/] CallExpression:matches([callee.name='refetch'], [callee.property.name='refetch'])",
          message:
            "Poll through the query's own refetchInterval with pollInterval() from @broccoli/web-sdk/hooks, never a timer that calls refetch().",
        },
        {
          // A server's `message` is English detail for logs; what the user
          // reads comes from useErrorMessage(), which translates by code.
          selector:
            "CallExpression:matches([callee.object.name='toast'], [callee.name=/^set(Error|Message|Notice)$/]) MemberExpression[property.name='message']",
          message:
            'Show a failure through useErrorMessage() from @broccoli/web-sdk/api, never its raw message.',
        },
        {
          selector:
            'Property[key.name=/^(label|title|description|placeholder|message|confirmLabel|failureMessage|emptyMessage)$/] > Literal[value=/[A-Za-z]{2}/]:not([value=/[a-z0-9]-[a-z]/]):not([value=/^[\\w-]+(\\.[\\w-]+)+$/])',
          message: 'UI text comes from a translation key: use t().',
        },
      ],
    },
  },
]);
