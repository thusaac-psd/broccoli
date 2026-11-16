# Documentation site

The Broccoli docs site, built with [Docusaurus](https://docusaurus.io/). Pages
live in `docs/`, with the Simplified Chinese mirror under
`i18n/zh-CN/docusaurus-plugin-content-docs/`.

Write pages against the guides in [`../docs/authoring`](../docs/authoring):
`house-style.md` for voice and structure, `diagrams.md` for diagrams.

## Develop

```bash
pnpm install
pnpm start
```

`pnpm start` serves the English site with hot reload. To preview the Chinese
locale, pass it explicitly:

```bash
pnpm start --locale zh-CN
```

## Build

```bash
pnpm build
pnpm serve
```

`pnpm build` writes the static site for every locale to `build/` and `pnpm serve`
serves that output. The build fails on a broken internal link, because the config
sets `onBrokenLinks: 'throw'`. Run `pnpm clear` first if a stale cache produces
results that do not match the source.

## Diagrams

Diagrams are generated, not drawn by hand in an editor. Edit the scene data and
regenerate:

```bash
pnpm diagrams
```

See [`diagrams/README.md`](diagrams/README.md) for the scene format and the dark
mode contract.

## Translations

Heading anchors for Chinese pages come from the CJK auto-anchor, so headings need
no explicit id. When you add UI strings or theme labels that Docusaurus owns,
regenerate the translation stubs:

```bash
pnpm write-translations --locale zh-CN
```

## Dependency advisories

The site has its own `pnpm-lock.yaml`, outside the root pnpm workspace, and the
weekly security audit checks it with `pnpm audit --prod`. `pnpm.overrides` in
`package.json` lifts transitive packages past advisories that Docusaurus's own
ranges do not reach yet. `pnpm.auditConfig.ignoreGhsas` lists the advisories
with no patched release:

- `GHSA-gcqx-w48m-w2h4` (deepmerge prototype poisoning, reached through
  `@svgr/plugin-svgo`): no deepmerge release fixes it, and deepmerge only
  merges the site's own build-time SVGO options, never input from a visitor.
