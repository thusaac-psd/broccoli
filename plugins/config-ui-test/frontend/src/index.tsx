// Plugin components. Export names must match keys in plugin.toml [web.components].
// The manifest (slots, routes) is defined in plugin.toml and served by the backend;
// this file only exports the React components and optional lifecycle hooks.

export { CollapsibleWrapper } from './CollapsibleWrapper';
export { ColorPickerField } from './ColorPickerField';
export { ModeCardSelector } from './ModeCardSelector';
