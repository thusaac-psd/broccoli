import { readFileSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import type { Plugin, ResolvedConfig, Rollup } from 'vite';

/**
 * Vite plugin that enables dynamically loaded plugins (from the backend) to
 * import shared dependencies like `react` via bare specifiers.
 *
 * It does two things:
 *   1. Exposes a virtual module (`virtual:shared-deps-map`) that returns
 *      a mapping of bare specifiers -> browser-resolvable URLs.
 *   2. Emits thin re-export "shim" modules for each shared dep. In dev via
 *      middleware (re-exporting from Vite's pre-bundled deps), in production via
 *      emitted chunks.
 *
 * The consumer (root.tsx) injects the map as a `<script type="importmap">`
 * in `<head>`, which the browser uses to resolve bare imports in any
 * dynamically loaded module (including cross-origin plugin bundles).
 */

/**
 * Every `@broccoli/web-sdk/*` subpath a plugin bundle may import must be listed
 * here: plugins externalize these specifiers and rely on the host's import map
 * to resolve them at runtime. A missing entry makes the plugin fail to load with
 * "Failed to resolve module specifier". Keep in sync with web-sdk's subpaths.
 */
export const SDK_SHARED_DEPS = [
  '@broccoli/web-sdk',
  '@broccoli/web-sdk/api',
  '@broccoli/web-sdk/auth',
  '@broccoli/web-sdk/contest',
  '@broccoli/web-sdk/hooks',
  '@broccoli/web-sdk/i18n',
  '@broccoli/web-sdk/permissions',
  '@broccoli/web-sdk/plugin',
  '@broccoli/web-sdk/problem',
  '@broccoli/web-sdk/sidebar',
  '@broccoli/web-sdk/slot',
  '@broccoli/web-sdk/submission',
  '@broccoli/web-sdk/theme',
  '@broccoli/web-sdk/ui',
  '@broccoli/web-sdk/utils',
] as const;

export const SHARED_DEPS = [
  'react',
  'react/jsx-runtime',
  'react/jsx-dev-runtime',
  'react-dom',
  'react-dom/client',
  'react-router',
  '@tanstack/react-query',
  ...SDK_SHARED_DEPS,
  'lucide-react',
  '@monaco-editor/react',
] as const;

/** CJS deps whose named exports hang off `module.exports` (the default). */
const DEFAULT_EXPORT_DEPS = new Set<(typeof SHARED_DEPS)[number]>([
  'react',
  'react/jsx-runtime',
  'react/jsx-dev-runtime',
  'react-dom',
  'react-dom/client',
]);

/**
 * ESM deps with a default export alongside real named exports. `export *`
 * skips `default`, so it is re-exported explicitly. (Treating these as CJS
 * would read `loader` etc. off the default `Editor` component, yielding
 * undefined.)
 */
const ESM_DEFAULT_EXPORT_DEPS = new Set<(typeof SHARED_DEPS)[number]>([
  '@monaco-editor/react',
]);

/**
 * Host modules (root-relative) a shim imports for side effects before
 * re-exporting its dep, so plugins get the same configured instance the host
 * uses even when no host component has loaded it yet.
 */
const SHIM_SETUP_MODULES: Partial<
  Record<(typeof SHARED_DEPS)[number], string>
> = {
  // Points the Monaco loader at the bundled copy instead of the jsdelivr CDN.
  '@monaco-editor/react': 'src/lib/monaco-setup.ts',
};

const VIRTUAL_MAP_ID = 'virtual:shared-deps-map';
const RESOLVED_MAP_ID = '\0' + VIRTUAL_MAP_ID;

const SHIM_PREFIX = 'shared-dep:';
const RESOLVED_SHIM_PREFIX = '\0' + SHIM_PREFIX;

/**
 * Sentinel string embedded in the client build's virtual module.
 * Replaced with the real import map in `generateBundle`, after chunk
 * filenames (with content hashes) are finalized.
 *
 * We match any quote style (double, single, backtick) because Rolldown
 * may output template literals instead of regular strings.
 */
const PLACEHOLDER_RE = /(["`'])__SHARED_DEPS_MAP__\1/;

const MANIFEST_FILENAME = 'shared-deps-map.json';

/**
 * URL prefix for dev-mode shim modules served via middleware.
 * These re-export from Vite's pre-bundled deps so dynamically loaded plugins
 * (which bypass Vite's import rewriting) can resolve bare specifiers.
 */
const DEV_SHIM_PREFIX = '/@shared-deps/';

/** Reverse lookup from flattened ids back to original dep names. */
const FLAT_TO_DEP = new Map(SHARED_DEPS.map((dep) => [flattenId(dep), dep]));

const IDENT_RE = /^[a-zA-Z_$][a-zA-Z0-9_$]*$/;
const SDK_DIST_ROOT = fileURLToPath(
  new URL('../../web-sdk/dist/', import.meta.url),
);

const nodeRequire = createRequire(import.meta.url);

function getNamedExports(dep: string): string[] {
  try {
    const mod = nodeRequire(dep);
    if (typeof mod !== 'object' || mod === null) return [];
    return Object.keys(mod).filter(
      (k) => k !== 'default' && k !== '__esModule' && IDENT_RE.test(k),
    );
  } catch {
    return [];
  }
}

/** Mirrors Vite's dep pre-bundling filename convention: `/` -> `_` */
function flattenId(dep: string): string {
  return dep.replace(/\//g, '_');
}

function isSdkDep(dep: string): dep is (typeof SDK_SHARED_DEPS)[number] {
  return dep === '@broccoli/web-sdk' || dep.startsWith('@broccoli/web-sdk/');
}

function resolveSdkEntry(dep: (typeof SDK_SHARED_DEPS)[number]): string {
  try {
    return fileURLToPath(import.meta.resolve(dep));
  } catch {
    if (dep === '@broccoli/web-sdk') {
      return resolve(SDK_DIST_ROOT, 'index.js');
    }

    return resolve(
      SDK_DIST_ROOT,
      dep.slice('@broccoli/web-sdk/'.length),
      'index.js',
    );
  }
}

function toFsModuleUrl(absPath: string): string {
  return `/@fs/${absPath.replace(/\\/g, '/').replace(/^\/+/, '')}`;
}

/**
 * Read the browserHash from Vite's dep optimizer metadata.
 * This hash must be appended as `?v={hash}` so the browser resolves to the
 * exact same module instance the host app uses (module cache keys on full URL).
 */
function readBrowserHash(cacheDir: string): string {
  try {
    const metaPath = resolve(cacheDir, 'deps', '_metadata.json');
    const meta = JSON.parse(readFileSync(metaPath, 'utf-8'));
    return meta.browserHash || '';
  } catch {
    return '';
  }
}

function readClientSharedDepsMap(
  config: ResolvedConfig,
): Record<string, string> {
  try {
    const manifestPath = resolve(
      config.root,
      'build',
      'client',
      MANIFEST_FILENAME,
    );
    const manifest = JSON.parse(readFileSync(manifestPath, 'utf-8'));
    if (typeof manifest === 'object' && manifest !== null) {
      return manifest;
    }
  } catch {
    // React Router's client build writes this before the server/static pass.
    // If another build mode runs without that client pass, fall through to an
    // empty map rather than emitting server-only chunk URLs into index.html.
  }

  return {};
}

function hoistImportMapInHtml(html: string): string {
  const importMapRe =
    /<script\b[^>]*\btype=["']importmap["'][^>]*>[\s\S]*?<\/script>/i;
  const importMapMatch = importMapRe.exec(html);
  if (!importMapMatch) return html;

  const headMatch = /<head(\s[^>]*)?>/i.exec(html);
  if (!headMatch) return html;

  const importMap = importMapMatch[0];
  const htmlWithoutImportMap =
    html.slice(0, importMapMatch.index) +
    html.slice(importMapMatch.index + importMap.length);
  const modulePreloadMatch =
    /<link\b(?=[^>]*\brel=["']modulepreload["'])[^>]*>/i.exec(
      htmlWithoutImportMap,
    );
  const insertAt = modulePreloadMatch
    ? modulePreloadMatch.index
    : headMatch.index + headMatch[0].length;

  return (
    htmlWithoutImportMap.slice(0, insertAt) +
    importMap +
    htmlWithoutImportMap.slice(insertAt)
  );
}

/** CSS files attached to a chunk and every chunk it statically imports. */
function collectStaticCss(
  bundle: Rollup.OutputBundle,
  entry: string,
): string[] {
  const css = new Set<string>();
  const seen = new Set<string>();
  const visit = (fileName: string) => {
    if (seen.has(fileName)) return;
    seen.add(fileName);
    const chunk = bundle[fileName];
    if (chunk?.type !== 'chunk') return;
    chunk.viteMetadata?.importedCss.forEach((file) => css.add(file));
    chunk.imports.forEach(visit);
  };
  visit(entry);
  return [...css];
}

function hoistClientIndexImportMap(config: ResolvedConfig): void {
  const indexPath = resolve(config.root, 'build', 'client', 'index.html');
  try {
    const html = readFileSync(indexPath, 'utf-8');
    const hoisted = hoistImportMapInHtml(html);
    if (hoisted !== html) {
      writeFileSync(indexPath, hoisted);
    }
  } catch {
    // Some build modes do not emit the React Router static index.
  }
}

/**
 * Generate a dev-mode ESM shim that re-exports a pre-bundled dependency.
 *
 * Vite pre-bundles CJS deps (React) as `export default module.exports` with
 * no named exports. ESM deps like `@tanstack/react-query` keep their real named
 * exports but have no default. We handle both via namespace import:
 *
 * ```
 *   import * as __ns gives `{ default: cjsExports }` for CJS
 *   and `{ useQuery, ... }` for ESM.
 * ```
 *
 * Then `__ns.default ?? __ns` gives the right object to extract names from.
 *
 * The `?v={browserHash}` query is critical, because without it, the browser would
 * load a separate module instance (URL mismatch leads to duplicate React and broken
 * hooks).
 */
function generateDevShim(dep: string, browserHash: string): string {
  const named = getNamedExports(dep);
  const hashSuffix = browserHash ? `?v=${browserHash}` : '';
  const importSource = isSdkDep(dep)
    ? toFsModuleUrl(resolveSdkEntry(dep))
    : `/node_modules/.vite/deps/${flattenId(dep)}.js${hashSuffix}`;

  const setupModule = SHIM_SETUP_MODULES[dep as (typeof SHARED_DEPS)[number]];
  const lines = [
    // Root-relative URL matches how the host's own imports resolve in dev,
    // so the browser shares one module instance.
    ...(setupModule ? [`import '/${setupModule}';`] : []),
    `import * as __ns from '${importSource}';`,
  ];

  if (DEFAULT_EXPORT_DEPS.has(dep as (typeof SHARED_DEPS)[number])) {
    lines.push(`const __mod = __ns.default;`);
    lines.push(`export default __mod;`);
    for (const name of named) {
      lines.push(`export const ${name} = __mod.${name};`);
    }
  } else {
    lines.push(`export * from '${importSource}';`);
    if (ESM_DEFAULT_EXPORT_DEPS.has(dep as (typeof SHARED_DEPS)[number])) {
      lines.push(`export { default } from '${importSource}';`);
    }
  }

  return lines.join('\n');
}

export function sharedDepsPlugin(): Plugin {
  let isDev = false;
  let config: ResolvedConfig;

  return {
    name: 'broccoli:shared-deps',

    config(_, { command }) {
      isDev = command === 'serve';
    },

    configResolved(resolved) {
      config = resolved;
    },

    configureServer(server) {
      // Force full page reload when Vite re-optimizes deps, because the
      // import map in <head> is immutable once parsed by the browser.
      // Without this, dynamically loaded plugins would resolve bare
      // specifiers to stale pre-bundle URLs (old browserHash), loading
      // duplicate module instances and breaking React context.
      let lastBrowserHash = '';
      server.watcher?.on('change', (path) => {
        if (path.endsWith('_metadata.json') && path.includes('.vite/deps')) {
          const newHash = readBrowserHash(config.cacheDir);
          if (lastBrowserHash && newHash && newHash !== lastBrowserHash) {
            server.ws.send({ type: 'full-reload' });
          }
          lastBrowserHash = newHash;
        }
      });

      server.middlewares.use((req, res, next) => {
        if (!req.url?.startsWith(DEV_SHIM_PREFIX)) return next();

        const browserHash = readBrowserHash(config.cacheDir);
        const reqPath = req.url.split('?', 1)[0];

        const depFlat = reqPath
          .slice(DEV_SHIM_PREFIX.length)
          .replace(/\.js$/, '');
        const dep = FLAT_TO_DEP.get(depFlat);

        if (!dep) {
          res.statusCode = 404;
          res.end('Not a shared dep');
          return;
        }

        const code = generateDevShim(dep, browserHash);
        res.setHeader('Content-Type', 'application/javascript');
        res.setHeader('Cache-Control', 'no-cache');
        res.end(code);
      });

      server.middlewares.use((_, res, next) => {
        const browserHash = readBrowserHash(config.cacheDir);
        const hashSuffix = browserHash ? `?v=${browserHash}` : '';
        const imports: Record<string, string> = {};
        for (const dep of SHARED_DEPS) {
          imports[dep] = `${DEV_SHIM_PREFIX}${flattenId(dep)}.js${hashSuffix}`;
        }
        const importMapTag = `<script type="importmap">${JSON.stringify({ imports })}</script>`;

        let injected = false;
        const _end = res.end.bind(res);
        const _write = res.write.bind(res);

        function tryInject(chunk: unknown, enc?: string): unknown {
          if (injected || chunk == null) return chunk;
          const ct = res.getHeader('content-type');
          if (!ct || !String(ct).includes('text/html')) return chunk;

          const encoding = (enc || 'utf-8') as BufferEncoding;
          const str =
            typeof chunk === 'string'
              ? chunk
              : Buffer.isBuffer(chunk)
                ? chunk.toString(encoding)
                : null;
          if (str === null) return chunk;

          const match = /<head(\s[^>]*)?>|<head>/.exec(str);
          if (!match) return chunk;

          injected = true;
          const at = match.index + match[0].length;
          const patched = str.slice(0, at) + importMapTag + str.slice(at);
          return typeof chunk === 'string'
            ? patched
            : Buffer.from(patched, encoding);
        }

        res.write = function (
          chunk: unknown,
          encOrCb?: BufferEncoding | ((err?: Error | null) => void),
          cb?: (err?: Error | null) => void,
        ): boolean {
          const enc = typeof encOrCb === 'string' ? encOrCb : undefined;
          const patched = tryInject(chunk, enc);
          if (typeof encOrCb === 'function')
            return _write(patched as string, encOrCb);
          return _write(patched as string, encOrCb as BufferEncoding, cb);
        } as typeof res.write;

        res.end = function (
          chunk?: unknown,
          encOrCb?: BufferEncoding | ((err?: Error | null) => void),
          cb?: (err?: Error | null) => void,
        ) {
          const enc = typeof encOrCb === 'string' ? encOrCb : undefined;
          const patched = tryInject(chunk, enc);
          if (typeof encOrCb === 'function')
            return _end(patched as string, encOrCb);
          return _end(patched as string, encOrCb as BufferEncoding, cb);
        } as typeof res.end;

        next();
      });
    },

    resolveId(id) {
      if (isDev && isSdkDep(id)) {
        return `${DEV_SHIM_PREFIX}${flattenId(id)}.js`;
      }
      if (id === VIRTUAL_MAP_ID) return RESOLVED_MAP_ID;
      if (id.startsWith(SHIM_PREFIX)) return '\0' + id;
      return null;
    },

    load(id) {
      // Virtual import map module
      if (id === RESOLVED_MAP_ID) {
        if (isDev) {
          return `export default {};`;
        }

        if (config.build.ssr) {
          const imports = readClientSharedDepsMap(config);
          return `export default ${JSON.stringify(imports)};`;
        }

        // For production:
        // embed a placeholder that generateBundle will replace
        // with the actual hashed chunk URLs.
        return [
          `var _m = "__SHARED_DEPS_MAP__";`,
          `export default typeof _m === "string" ? {} : _m;`,
        ].join('\n');
      }

      if (id.startsWith(RESOLVED_SHIM_PREFIX)) {
        const dep = id.slice(RESOLVED_SHIM_PREFIX.length);
        const setupModule =
          SHIM_SETUP_MODULES[dep as (typeof SHARED_DEPS)[number]];
        const setupImports = setupModule
          ? [`import ${JSON.stringify(resolve(config.root, setupModule))};`]
          : [];
        if (!DEFAULT_EXPORT_DEPS.has(dep as (typeof SHARED_DEPS)[number])) {
          const reexports = [`export * from '${dep}';`];
          if (
            ESM_DEFAULT_EXPORT_DEPS.has(dep as (typeof SHARED_DEPS)[number])
          ) {
            reexports.push(`export { default } from '${dep}';`);
          }
          return [...setupImports, ...reexports].join('\n');
        }

        const named = getNamedExports(dep);
        const lines = [
          ...setupImports,
          `import * as __ns from '${dep}';`,
          `const __mod = __ns.default;`,
          `export default __mod;`,
        ];
        for (const name of named) {
          lines.push(`export const ${name} = __mod.${name};`);
        }
        return lines.join('\n');
      }
    },

    buildStart() {
      if (isDev) return;

      for (const dep of SHARED_DEPS) {
        this.emitFile({
          type: 'chunk',
          id: `${SHIM_PREFIX}${dep}`,
          name: `shared-${flattenId(dep)}`,
        });
      }
    },

    generateBundle(_, bundle) {
      if (isDev) return;

      const base = config.base || '/';
      const imports: Record<string, string> = {};

      for (const [filename, chunk] of Object.entries(bundle)) {
        if (chunk.type !== 'chunk' || !chunk.facadeModuleId) continue;
        if (!chunk.facadeModuleId.startsWith(RESOLVED_SHIM_PREFIX)) continue;

        const dep = chunk.facadeModuleId.slice(RESOLVED_SHIM_PREFIX.length);
        imports[dep] = `${base}${filename}`;

        // Plugins enter shims through the import map, bypassing Vite's
        // preload helper that normally attaches a chunk's CSS (e.g. Monaco's
        // editor styles), so the shim links its static graph's CSS itself.
        const cssHrefs = collectStaticCss(bundle, filename).map(
          (css) => `${base}${css}`,
        );
        if (cssHrefs.length > 0) {
          chunk.code =
            `for(const h of ${JSON.stringify(cssHrefs)})if(!document.querySelector('link[rel="stylesheet"][href="'+h+'"]')){const l=document.createElement('link');l.rel='stylesheet';l.href=h;document.head.appendChild(l)}\n` +
            chunk.code;
        }
      }

      const replacement = JSON.stringify(imports);
      for (const chunk of Object.values(bundle)) {
        if (chunk.type === 'chunk' && PLACEHOLDER_RE.test(chunk.code)) {
          chunk.code = chunk.code.replace(PLACEHOLDER_RE, () => replacement);
        }
      }

      // Also emit as a standalone JSON asset (useful for debugging / tooling)
      this.emitFile({
        type: 'asset',
        fileName: MANIFEST_FILENAME,
        source: JSON.stringify(imports, null, 2),
      });
    },

    writeBundle() {
      if (isDev || config.build.ssr) return;

      hoistClientIndexImportMap(config);
    },

    closeBundle() {
      if (isDev) return;

      hoistClientIndexImportMap(config);
    },
  };
}
