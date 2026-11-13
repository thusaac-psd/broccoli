import { use } from 'react';

import { PluginRegistryContext } from '@/plugin/registry/plugin-registry-context';

export function usePluginRegistry() {
  const context = use(PluginRegistryContext);
  if (!context) {
    throw new Error(
      'usePluginRegistry must be used within PluginRegistryProvider',
    );
  }
  return context;
}
