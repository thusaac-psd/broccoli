# @broccoli/web-sdk

The frontend SDK for Broccoli. It carries three things: the plugin slot system
that lets plugins inject React components into the app, a shared UI kit so
plugins match the host, and the API, i18n, theming, and auth helpers a plugin
frontend needs. The Broccoli web app and every plugin frontend build against it.

## Install

```bash
pnpm add @broccoli/web-sdk
```

Inside this monorepo a plugin frontend is a pnpm workspace member and depends on
it as `workspace:*`. See `plugins/icpc/frontend` and `plugins/print/frontend`
for working setups.

## Entry points

The root export is empty. Import from a subpath:

| Import                              | What it provides                                     |
| ----------------------------------- | ---------------------------------------------------- |
| `@broccoli/web-sdk/plugin`          | `PluginRegistryProvider`, `usePluginRegistry`, types |
| `@broccoli/web-sdk/slot`            | The `Slot` component                                 |
| `@broccoli/web-sdk/ui`              | UI kit: `Button`, `DataTable`, `Dialog`, `Tabs`, …   |
| `@broccoli/web-sdk/api`             | `useApiFetch`, the API client, `serverNow`           |
| `@broccoli/web-sdk/hooks`           | `useIdempotentRequest`, `useServerTable`, …          |
| `@broccoli/web-sdk/i18n`            | Translation hooks and types                          |
| `@broccoli/web-sdk/auth`            | Auth state                                           |
| `@broccoli/web-sdk/permissions`     | Permission name constants                            |
| `@broccoli/web-sdk/contest`         | Contest types                                        |
| `@broccoli/web-sdk/problem`         | Problem types and presentation                       |
| `@broccoli/web-sdk/submission`      | Submission types and helpers                         |
| `@broccoli/web-sdk/clarification`   | Clarification types                                  |
| `@broccoli/web-sdk/sidebar`         | Sidebar state                                        |
| `@broccoli/web-sdk/theme`           | Theme state                                          |
| `@broccoli/web-sdk/utils`           | `cn`, date and number formatting                     |
| `@broccoli/web-sdk/tailwind-preset` | Tailwind preset for plugin styles                    |
| `@broccoli/web-sdk/plugin.css`      | Base plugin stylesheet                               |

## How slots work

A slot is a named injection point in the UI. The host renders a `Slot`, and any
active plugin that targets that slot has its component rendered there.

```tsx
import { Slot } from '@broccoli/web-sdk/slot';

<Slot name="submission-result.rejection" slotProps={{ submission }} />;
```

A plugin does not register slots in frontend code. It declares them in its
`plugin.toml`, exports the named component, and the server reports it as active.
The provider fetches active plugins from `backendUrl` and wires them up. Slots
carry an optional `permission` and `contest_type`, and `Slot` renders a plugin's
component only when the current user and contest satisfy them.

## Set up the provider

Wrap the app once. `backendUrl` is where the registry fetches active plugins.
`lazyPlugins` is for plugins bundled with the app and code-split with dynamic
`import()`; remote plugins delivered by the server need no entry here.

```tsx
import { PluginRegistryProvider } from '@broccoli/web-sdk/plugin';

function App() {
  return (
    <PluginRegistryProvider backendUrl="http://127.0.0.1:3000">
      <YourApp />
    </PluginRegistryProvider>
  );
}
```

## UI kit and styles

Build plugin UI from `@broccoli/web-sdk/ui` so it matches the app: `Button`,
`Badge`, `Card`, `Dialog`, `Select`, `Tabs`, `Textarea`, `DataTable`,
`FileDropZone`, `Sidebar`, `Sonner` toasts, and more. Use the Tailwind preset
and the plugin stylesheet to pick up the same tokens:

```ts
// tailwind config
import preset from '@broccoli/web-sdk/tailwind-preset';
export default { presets: [preset] };
```

## Where to go next

The end-to-end plugin walkthrough lives on the docs site under Building plugins.
`plugins/icpc/frontend` and `plugins/print/frontend` are the reference
frontends.

## License

MIT
