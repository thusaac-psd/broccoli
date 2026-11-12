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

  // User-facing text comes from translation keys, never from JSX literals:
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
              'aria-label',
              'alt',
              'label',
              'description',
              'emptyMessage',
              'searchPlaceholder',
              'tooltip',
              'hint',
            ],
          },
          words: {
            exclude: [
              // Numbers, punctuation and symbols.
              '[0-9!-/:-@[-`{-~\\s·—–→←×✓…▾▸]+',
              // Constants and codes.
              '[A-Z_-]+',
              // Colors.
              '#[0-9a-fA-F]{3,8}',
              'var\\(--[a-z-]+\\)',
              // Class names.
              '[a-z0-9:./\\[\\]()!%#_-]*-[a-z0-9:./\\[\\]()!%#_ -]*',
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
]);
