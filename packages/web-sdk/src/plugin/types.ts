/**
 * Core type definitions
 */

import type { ElementType, ReactNode } from 'react';

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
export type PluginStatus = components['schemas']['PluginStatusResponse'];
export type PluginSource = components['schemas']['PluginSourceResponse'];
export type PluginArchiveSummary =
  components['schemas']['PluginArchiveSummary'];

export interface ComponentBundle {
  [key: string]: ElementType;
}

export interface SlotRenderContext {
  slotName: string;
  children?: ReactNode;
  props?: Record<string, unknown>;
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

/**
 * A lazy plugin loader: a function that returns a promise resolving to a
 * PluginModule. Used for code-splitting plugins so they are only fetched
 * when the application mounts rather than being included in the main bundle.
 */
export type LazyPluginLoader = () => Promise<PluginModule>;
