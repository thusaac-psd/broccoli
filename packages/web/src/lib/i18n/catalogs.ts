import type { Catalog } from './catalog.ts';
import { en } from './en.ts';
import { zhCN } from './zh-CN.ts';

/**
 * The host catalogs shipped in the bundle, by locale. The UI speaks these
 * languages from the first paint, and with the server unreachable; plugin
 * catalogs arrive from the server and add to them.
 */
export const CORE_CATALOGS: Record<string, Catalog> = { en, 'zh-CN': zhCN };
