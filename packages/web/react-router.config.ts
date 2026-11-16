import { join } from 'node:path';

import type { Config } from '@react-router/dev/config';

import { precompressDir } from './plugins/precompress.ts';

export default {
  appDirectory: 'src',
  ssr: false,
  // Once every build is done, React Router's own `manifest-*.js` included
  // (written after Vite's bundle closes), the client's content-hashed assets
  // get compressed copies. `index.html` is small and not hashed, so the
  // server compresses it per request.
  async buildEnd({ reactRouterConfig, viteConfig }) {
    await precompressDir(
      join(
        reactRouterConfig.buildDirectory,
        'client',
        viteConfig.build.assetsDir,
      ),
    );
  },
} satisfies Config;
