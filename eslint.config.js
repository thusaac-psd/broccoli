import js from '@eslint/js';
import { defineConfig, globalIgnores } from 'eslint/config';
import eslintConfigPrettier from 'eslint-config-prettier/flat';
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
]);
