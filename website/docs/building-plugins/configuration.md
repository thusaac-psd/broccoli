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
satisfies its schema. An admin's write must also pass the namespace's
`validate` function, when it declares one (see below):

| Response | Code | When |
| --- | --- | --- |
| 404 | `CONFIG_NAMESPACE_NOT_FOUND` | the plugin is not installed, declares no such namespace, or not for this scope |
| 400 | `VALIDATION_ERROR` | the value breaks the schema, or the namespace's `validate` function refuses it; `details.fields` lists each `{path, message, kind}` (`kind: "plugin"` with an optional `key` and `params` for a `validate` refusal) |
| 503 | `PLUGIN_UNAVAILABLE` | the namespace has a `validate` function and the plugin cannot answer right now |

A stored value may leave fields out: they read as their defaults. When an
upgrade changes a schema, inspecting or uploading the archive lists every
stored config the new schema refuses under `incompatible_configs`.

## Refuse what a schema cannot say

A schema checks types and bounds, not meaning. A subtask list can be
well-typed and still name a test case the problem does not have, which would
score every submission 0. Name a function under `validate`, and the host calls
it on every admin write to the namespace, after the schema check and before it
stores anything:

```toml
[config.task]
scopes = ["contest_problem"]
validate = "validate_task_config"
```

The function receives a `ConfigValidateInput`: the namespace, the scope, the
`contest_id` and `problem_id` the config belongs to, and the proposed value
with its defaults filled in. Read whatever else you need through your usual
host functions, and answer with a `ConfigValidateOutput` listing one
`ConfigViolation` per problem. An empty list accepts the value:

```rust
#[plugin_fn]
pub fn validate_task_config(input: String) -> FnResult<String> {
    let host = Host::new();
    let req: ConfigValidateInput = serde_json::from_str(&input)?;
    let task: TaskConfig = req.parse()?;
    let problem_id = req
        .problem_id
        .ok_or_else(|| SdkError::Other("a task config belongs to a problem".into()))?;
    let labels = host.submission.query_test_cases(problem_id)?;
    let errors = task
        .subtasks
        .iter()
        .enumerate()
        .flat_map(|(i, s)| s.test_cases.iter().enumerate().map(move |(j, l)| (i, j, l)))
        .filter(|(_, _, label)| !labels.iter().any(|tc| tc.label.as_deref() == Some(label)))
        .map(|(i, j, label)| {
            ConfigViolation::new(
                format!("subtasks[{i}].test_cases[{j}]"),
                format!("this problem has no test case '{label}'"),
            )
            .key("my-plugin.config.task.unknownTestCase")
            .param("testCase", label.as_str())
        })
        .collect();
    Ok(serde_json::to_string(&ConfigValidateOutput { errors })?)
}
```

The host answers each violation as a field of the `VALIDATION_ERROR`, so the
settings form shows it next to the field its `path` names and lists it with
the save error. A violation with a `key` reads in the user's language: the
form looks the key up in your plugin's translation pack and fills in its
`params`, and shows your `message` when the pack has no translation. The key
must start with your plugin's id (`my-plugin.`); the host drops any other key,
so a plugin cannot show the host's strings or another plugin's. A function that
fails or returns something unreadable refuses the write too: the host never
stores a value its validator did not accept.

Writes your plugin makes with `host.config.set*` are checked against the
schema only. The plugin owns those values, so the host does not call its
validator on them.

The test data a config refers to can change after the config is stored. Every
change to a problem's test cases (create, edit, delete, reorder, bulk delete
and ZIP upload) therefore runs every config stored for the problem, at the
problem scope and in each contest that uses it, through the same checks
before it commits, with `host.submission.query_test_cases` answering with the
test cases as the change leaves them. A change that gives a config an issue
is refused with 409 `CONFIG_WOULD_BREAK`, its `details.issues` in the
`incompatible_configs` shape, unless the request carries `force=true`. Your
own SQL (`host.db.*`) reads committed data during that check, so read test
cases through `host.submission.query_test_cases` in a validator.

`GET /api/v1/problems/{id}/config/issues` lists the configs refused now. The
problem's test case dialog shows them, and a judgement dispatched while there
are any says so to staff (`config_issues` on the submission's judgements).

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

## Hold a problem's checker source

A checker plugin whose formats compile a problem's own checker (testlib, for
example) declares one problem-scoped namespace with `checker_source = true`.
The host's checker source endpoints store the files an admin uploads there, as
`{ "files": [{ "filename", "content" }] }`, for every problem whose checker
format the plugin provides, and the problem editor's checker source page shows
them. The namespace is not offered as a setting anywhere else.

```toml
[config.checker_source]
scopes = ["problem"]
checker_source = true

[config.checker_source.properties.files]
type = "array"
default = []
```

The plugin reads it like any other problem config:
`host.config.get_problem(problem_id, "checker_source")`. A manifest may mark at
most one namespace, and only with `scopes = ["problem"]`.

## Test it

`Host::mock()` reads the `plugin.toml` of the crate under test, so a native
test sees the same filled in config as the server:

```rust
let host = Host::mock();
host.config.seed("contest", "2", "contest", json!({"freeze_minutes": 15}));
let config: ContestConfig = host.config.get_contest(2, "contest")?.parse()?;
assert_eq!(config.penalty_minutes, 20);
```
