---
title: Configuration
sidebar_label: Configuration
sidebar_position: 2
---

# Configuration

A plugin's settings are declared once, as a schema in `plugin.toml`. The host
owns everything else: it renders the settings form from the schema, checks
every value written against it, and hands your code the stored value with the
schema's defaults filled in. Your Rust config type therefore carries field
names and types only, never defaults or bounds.

## Declare a namespace

Each `[config.<namespace>]` table is one group of settings. `scopes` lists
where it can be set: `plugin` (global), `problem`, `contest` and
`contest_problem`.

```toml
[config.contest]
description = "ICPC contest-level configuration"
scopes = ["contest"]

[config.contest.properties.penalty_minutes]
type = "integer"
title = "Penalty Minutes"
default = 20
min = 0
max = 1440

[config.contest.properties.freeze_minutes]
type = "integer"
title = "Scoreboard Freeze (minutes)"
default = 0
min = 0
```

A property's `type` is one of `string`, `integer`, `number`, `boolean`,
`array` and `object`. The host enforces `min` and `max` for numbers,
`min_length`, `max_length` and `pattern` for strings, `enum`, the `items` of
an array, and the `properties` and `required` fields of an object. An object
that declares properties refuses fields it does not declare, unless it sets
`additional_properties = true`. `step`, `precision`, `unit` and `span` only
shape the form.

A manifest whose defaults break its own schema, or that names an unknown
type, does not load.

## What the host refuses

A write through the admin API, or through `host.config.set*`, is refused
unless the plugin declares the namespace for that scope and the value
satisfies its schema:

| Response | Code | When |
| --- | --- | --- |
| 404 | `CONFIG_NAMESPACE_NOT_FOUND` | the plugin is not installed, declares no such namespace, or not for this scope |
| 400 | `VALIDATION_ERROR` | the value breaks the schema; `details.fields` lists each `{path, message}` |

A stored value may leave fields out: they read as their defaults. When an
upgrade changes a schema, inspecting or uploading the archive lists every
stored config the new schema refuses under `incompatible_configs`.

## Read it

The host fills in every default the schema declares, inside nested objects
and array items too. Deserialize into a type with no `Default` and no
`#[serde(default)]`, and propagate a parse error instead of falling back:

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct ContestConfig {
    pub penalty_minutes: i32,
    pub freeze_minutes: i32,
}

let config: ContestConfig = host.config.get_contest(contest_id, "contest")?.parse()?;
```

`EffectiveConfig::parse_config` does the same for the cascading
`host.config.get_effective`. A type that no longer matches its schema then
fails loudly where it is read.

## Test it

`Host::mock()` reads the `plugin.toml` of the crate under test, so a native
test sees the same filled in config as the server:

```rust
let host = Host::mock();
host.config.seed("contest", "2", "contest", json!({"freeze_minutes": 15}));
let config: ContestConfig = host.config.get_contest(2, "contest")?.parse()?;
assert_eq!(config.penalty_minutes, 20);
```
