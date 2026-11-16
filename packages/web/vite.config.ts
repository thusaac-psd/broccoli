import { reactRouter } from '@react-router/dev/vite';
import tailwindcss from '@tailwindcss/vite';
import path from 'path';
import { defineConfig } from 'vite';

import {
  resolveSdkEntry as resolveSdkDep,
  SDK_SHARED_DEPS,
  SHARED_DEPS,
  sharedDepsPlugin,
} from './plugins/shared-deps';
import { languagePacksPlugin } from './scripts/language-packs';

// https://vite.dev/config/
export default defineConfig(() => {
  const sdkAliases = [
    ...SDK_SHARED_DEPS.filter((dep) => dep !== '@broccoli/web-sdk').map(
      (dep) => ({
        find: dep,
        replacement: resolveSdkDep(dep),
      }),
    ),
    {
      find: /^@broccoli\/web-sdk$/,
      replacement: resolveSdkDep('@broccoli/web-sdk'),
    },
  ];

  return {
    plugins: [
      languagePacksPlugin(),
      tailwindcss(),
      sharedDepsPlugin(),
      reactRouter(),
    ],
    server: {
      proxy: {
        '/api': 'http://localhost:3000',
        '/assets': 'http://localhost:3000',
      },
    },
    build: {
      outDir: 'build/client',
    },
    resolve: {
      dedupe: ['react', 'react-dom'],
      alias: [
        { find: '@', replacement: path.resolve(__dirname, './src') },
        ...sdkAliases,
      ],
    },
    optimizeDeps: {
      include: [
        ...SHARED_DEPS.filter((dep) => !dep.startsWith('@broccoli/web-sdk')),
        'monaco-editor',
        'react-markdown',
        'katex',
        'rehype-katex',
        'remark-gfm',
        'remark-math',
      ],
      exclude: [...SDK_SHARED_DEPS],
    },
  };
});
