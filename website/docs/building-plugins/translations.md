---
title: Translating a plugin
sidebar_label: Translations
sidebar_position: 2
---

# Translating a plugin

The web UI speaks the language the user picked. A plugin ships a catalog per
language, and the host looks up the plugin's own text in it: the strings its
frontend shows, the names of what it registers, the reasons it refuses a
request, and the text of its manifest and config schemas. The host knows none
of these keys; it only knows where to look.

## Ship a catalog per language

List the catalogs in `plugin.toml`. `en` is the source; every other catalog
translates exactly its keys, with the same placeholders.

```toml
[translations]
en = "i18n/en.toml"
zh-CN = "i18n/zh-CN.toml"
```

```toml
# plugins/submission-limit/i18n/en.toml
'limit.usedCount' = '{used} / {total} used'
```

```toml
# plugins/submission-limit/i18n/zh-CN.toml
'limit.usedCount' = '已用 {used} / {total}'
```

```toml
# plugins/cooldown/i18n/en.toml
'apiError.COOLDOWN_ACTIVE' = '{remaining_seconds, plural, one {Wait # more second before submitting again.} other {Wait # more seconds before submitting again.}}'
```

A frontend reads them with `t()` from `useTranslation()` in
`@broccoli/web-sdk/i18n`. `{name}` inserts a parameter.
`{count, plural, one {...} other {...}}` picks the form the language's plural
rules choose for `count`, and `#` inside a branch is the number. Dates and
relative times come from `useFormat()`, which formats for the user's language;
measure server timestamps against `useFormat().relative`, which uses the
server's clock.

## Name what you register

The host shows contest types, problem types and checker formats by name. Add
the name under `identifier.<kind>.<id>`; without one the id itself is shown.

| What you register | Key |
| --- | --- |
| `register_contest_type("icpc", ...)` | `identifier.contestType.icpc` |
| `register_staged_evaluator("batch", ...)` | `identifier.problemType.batch` |
| `register_checker_resolver("exact", ...)` | `identifier.checker.exact` |

```toml
'identifier.contestType.icpc' = 'ICPC'
```

## Translate your refusals

A hook that rejects a submission returns a machine-readable code. The UI shows
the translation of `apiError.<CODE>`, with the rejection's `details` as
parameters, and never the English `message`:

```rust
HookResponse::reject(
    "SUBMISSION_LIMIT_EXCEEDED",
    format!("Submission limit reached ({count}/{max})"),
    429,
    Some(serde_json::json!({ "submissions_made": count, "max_submissions": max })),
)
```

```toml
'apiError.SUBMISSION_LIMIT_EXCEEDED' = 'You have used all {max_submissions} submissions for this problem.'
```

A code with no translation falls back to a generic message for its HTTP status
class. The same lookup applies to errors from your plugin's own HTTP routes.

## Translate your manifest and config schemas

The plugin list and the config forms show the manifest's `name` and
`description` and the schema's `title` and `description` fields. Each has a key
in your catalog; a missing key shows the text from `plugin.toml`.

| Text | Key |
| --- | --- |
| Plugin name | `plugin.<plugin id>.name` |
| Plugin description | `plugin.<plugin id>.description` |
| Config schema description | `config.<plugin id>.<namespace>.description` |
| Field title | `config.<plugin id>.<namespace>.<field path>.title` |
| Field description | `config.<plugin id>.<namespace>.<field path>.description` |

The field path joins nested property names with dots, so the field
`[config.sandbox.properties.compile_time_limit_s]` in `batch-evaluator` has
the title key `config.batch-evaluator.sandbox.compile_time_limit_s.title`.

```toml
'plugin.batch-evaluator.description' = 'Judges batch problems: compile, run against each test, check the output.'
'config.batch-evaluator.sandbox.description' = 'Sandbox resource limits for compilation and execution steps'
```
