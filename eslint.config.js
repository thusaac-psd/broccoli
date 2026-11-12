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

export default defineConfig([
  // Base config for all files
  globalIgnores([
    '**/dist/*',
    '**/coverage/*',
    'packages/web-sdk/src/api/schema.ts',
    'packages/web/.react-router/**',
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
        { allowConstantExport: true },
      ],
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
          selector:
            'Property[key.name=/^(label|title|description|placeholder|message|confirmLabel|failureMessage|emptyMessage)$/] > Literal[value=/[A-Za-z]{2}/]:not([value=/[a-z0-9]-[a-z]/]):not([value=/^[\\w-]+(\\.[\\w-]+)+$/])',
          message: 'UI text comes from a translation key: use t().',
        },
      ],
    },
  },
]);
