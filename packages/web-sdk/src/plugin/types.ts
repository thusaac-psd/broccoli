/**
 * Core type definitions
 */

import type { ElementType } from 'react';

import type { components } from '@/api/schema';
import type { TranslationMap } from '@/i18n/types';

export type ActivePluginManifest =
  components['schemas']['ActivePluginResponse'] & {
    /** Translations provided by local plugins (not part of the API schema). */
    translations?: Record<string, TranslationMap>;
  };
export type SlotConfig = components['schemas']['WebSlotConfig'] & {
  _pluginName: string;
};
export type RouteConfig = components['schemas']['WebRouteConfig'];

export type PluginDetail = components['schemas']['PluginDetailResponse'];
export type PluginFullDetail =
  components['schemas']['PluginFullDetailResponse'];
export interface ComponentBundle {
  [key: string]: ElementType;
}

export interface PluginModule {
  manifest: ActivePluginManifest;
  /**
   * Plugin initialization function called when plugin is registered
   */
  onInit?: () => void | Promise<void>;
  /**
   * Plugin cleanup function called when plugin is unregistered
   */
  onDestroy?: () => void | Promise<void>;
  /** Dynamic component exports keyed by name */
  [key: string]: unknown;
}
