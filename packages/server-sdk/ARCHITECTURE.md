# broccoli-server-sdk Architecture

## Design Principle

The SDK provides **building blocks**, not frameworks. If a feature is specific
to one contest type (e.g., subtasks for IOI, stop-on-failure for ICPC), it lives
entirely in that plugin. The SDK only adds abstractions that serve ALL plugins.

**What belongs here:** types, host function wrappers, error types, DB helpers
(queries + persistence), evaluator interpretation utilities.

**What does NOT belong here:** contest-specific strategies, plugin-shaped trait
hierarchies, orchestration runners that hardcode a particular pipeline flow.

## Plugin-Specific Data

Plugins that need to store configuration beyond what the core schema provides
(e.g., subtask groupings, custom scoring parameters) have several options:

### Plugin KV Storage

Use `store_get`/`store_set` host functions, keyed by convention (e.g.,
`problem:{id}:subtasks`).

- **Pro:** Already exists, plugin-isolated, no schema changes.
- **Con:** Orphaned on problem delete, no standard UI for editing.

### Problem Metadata JSON Field

A `plugin_config: Option<serde_json::Value>` column on the problem entity.

- **Pro:** Lifecycle tied to the problem, deleted automatically.
- **Con:** No validation at the DB level, potential key conflicts between
  plugins.
- **Precedent:** The `checker` field on problem is already
  `Option<serde_json::Value>`.

### Plugin-Created Tables

Plugins can use `db_execute("CREATE TABLE ...")` to create their own tables.
They land in the plugin's own schema, `plugin_<id>`, where the plugin has full
DDL and DML; host tables are read-only to it, so foreign keys can only point at
the plugin's own tables.

- **Pro:** Full SQL capabilities, constraints and indexing within the plugin's
  schema.
- **Con:** Migration management complexity, cleanup responsibility.

## UI for Plugin Data

- The frontend slot system already supports plugin-registered components.
- A plugin could register a "problem settings" panel for subtask configuration.
- Alternative: generic JSON editor for the problem metadata field.

## Adding New SDK Modules

Before adding a new module, verify it satisfies these criteria:

1. **Used by 2+ plugins** (or clearly universal, like error handling).
2. **Provides building blocks**, not an opinionated workflow.
3. **No plugin-specific defaults** baked into trait implementations.
