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
shape the form; `secret` hides a value (see below).

A manifest whose defaults break its own schema, or that names an unknown
type, does not load.

## What the host refuses

A write through the admin API, or through `host.config.set*`, is refused
unless the plugin declares the namespace for that scope and the value
satisfies its schema. Every write, your plugin's own included, must also
pass the namespace's `validate` function, when it declares one (see below):

| Response | Code | When |
| --- | --- | --- |
| 404 | `CONFIG_NAMESPACE_NOT_FOUND` | the plugin is not installed, declares no such namespace, or not for this scope |
| 400 | `VALIDATION_ERROR` | the value breaks the schema, or the namespace's `validate` function refuses it; `details.fields` lists each `{path, message, kind}` (`kind: "plugin"` with an optional `key` and `params` for a `validate` refusal) |
| 503 | `PLUGIN_UNAVAILABLE` | the namespace has a `validate` function and the plugin cannot answer right now |

A stored value may leave fields out: they read as their defaults. When an
upgrade changes a schema, inspecting or uploading the archive lists every
stored config the new schema refuses under `incompatible_configs`.

A write that leaves out the row's `enabled` toggle or its `position` keeps
what the stored row has; `"enabled": null` clears the toggle, so the level
above decides.

## Keep a secret

Mark a string field `secret = true` when its value is a credential, such as a
token a print station signs in with. An array whose items hold a secret names
its items by a key field with `key`: a string each item requires, which is not
itself a secret. A manifest whose array holds a secret without one does not
load.

```toml
[config.print.properties.stations]
type = "array"
key = "name"

[config.print.properties.stations.items]
type = "object"
required = ["name", "token"]

[config.print.properties.stations.items.properties.token]
type = "string"
title = "Token"
secret = true
```

The JSON schema marks the field `writeOnly` and the array `x-key`. No read
route sends a secret's value. The admin config routes answer a set secret as
its ref, `{"$secret": "<ref>"}` (the API schema's `KeptSecret`): an opaque id
the server mints each time it stores a new value, and keeps while the value
stays. The cascade answers the field as `null`.

A write keeps a stored secret only where it sends that ref back, on the item
the read showed it on (the stored item whose key field has the same value,
never the same position). Saving the config back as read keeps every
station's token whatever stations were added, removed or moved. Anything else
is the writer's own:

- A string sets the secret. Sending the stored value again keeps its ref; any
  other value gets a new one.
- `{"$clear": true}` (the API schema's `ClearSecret`) clears it, so the field
  reads unset.
- No secret (`null`, the empty string, or the field left out) where one is
  stored is refused with a `secret_not_stored` field error, and so is a ref
  the stored secret at that item does not hold: the writer never read that
  secret. A client that deletes a station and adds one of the same name, a
  station renamed, or a ref read before the secret was set again, gets
  nothing back. Where nothing is stored, no secret leaves the field unset.
- A secret its item requires is set or kept, never cleared or left without
  one: the marker, or no secret, is refused with a `secret_required` field
  error.

Two items of a keyed array with the same key are refused with a
`duplicate_key` field error at the second.

The settings form shows a set secret as an empty password field that sends
its ref back unless you type a new value, with a Clear action that sends the
marker (none for a required secret). An item the form adds, or one whose key
you change, is new even if a saved item had that key: its secret is entered,
and a blank one is sent as the marker, so no saved item's secret passes to it.

A config whose namespace this server knows no schema for (its plugin is not
installed here, or no longer declares the namespace), or whose plugin this
server does not run at the version the fleet published, cannot say which of
its fields are secret, so every read withholds it whole: `config` is `null`
and `config_withheld` is `true`, and the cascade shows none of its scopes.

A namespace may also declare a short `title`, which labels its tab in the
settings form; the namespace's id labels it otherwise.

Your plugin reads the real value: `host.config.get*` and the cascade a plugin
reads are not redacted. A secret can still read unset, for example in a config
a bundle restored onto another server, so read it with a default and treat an
unset secret as granting nothing.

## Refuse what a schema cannot say

A schema checks types and bounds, not meaning. A subtask list can be
well-typed and still name a test case the problem does not have, which would
score every submission 0. Name a function under `validate`, and the host calls
it on every write to the namespace, after the schema check and before it
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
`ConfigViolation` per problem. Empty lists accept the value:

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
    let issues = task
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
    Ok(serde_json::to_string(&ConfigValidateOutput {
        issues,
        ..Default::default()
    })?)
}
```

The answer has two lists, and which one a violation goes in decides whether
the host stores the value:

- `errors` is what is wrong with the config by itself, whatever else the
  server holds, such as two subtasks with one name. The host refuses the
  write.
- `issues` is everything that depends on other data: a subtask naming a test
  case the problem does not have, a problem with no test cases, more possible
  qualifiers than the contest's problems leave places for. The host stores
  the value and lists each issue on the target's config issues until the
  config or the data changes.

A rule that reads anything besides the proposed value (the test data, the
contest's problems, another scope's config) belongs in `issues`. Refusing it
would make every later save fail, an unrelated field's included, once the
data moves past it, and staff can fix the data and the config in either
order.

The host answers each violation as a field of the `VALIDATION_ERROR`, so the
settings form shows it next to the field its `path` names and lists it with
the save error. A violation with a `key` reads in the user's language: the
form looks the key up in your plugin's translation pack and fills in its
`params`, and shows your `message` when the pack has no translation. The key
must start with your plugin's id (`my-plugin.`); the host drops any other key,
so a plugin cannot show the host's strings or another plugin's. A function that
fails or returns something unreadable refuses the write too: the host never
stores a value its validator did not accept.

Writes your plugin makes with `host.config.set*` pass the same checks: the
host calls your validator as a call nested under the one that writes, and a
write it refuses fails that call, as any refused write does. A validator that
itself writes configs nests again, so keep config writes out of validators.

The test data a config refers to can change after the config is stored. Every
change to a problem's test cases (create, edit, delete, reorder, bulk delete
and ZIP upload) therefore runs every config stored for the problem, at the
problem scope and in each contest that uses it, through the same checks
before it commits, with `host.submission.query_test_cases` answering with the
test cases as the change leaves them. A change that gives a stored config an
error or an issue it did not have is refused with 409 `CONFIG_WOULD_BREAK`,
its `details.issues` in the `incompatible_configs` shape, unless the request
carries `force=true`. What a namespace's defaults find where nothing is
stored, such as a problem with no test cases yet, never refuses a change. Your
own SQL (`host.db.*`) reads committed data during that check, so read test
cases through `host.submission.query_test_cases` in a validator.

`GET /api/v1/problems/{id}/config/issues` lists what your plugin finds now,
errors and issues alike, for each config of the problem, at the problem scope
and in each contest that uses it. `GET /api/v1/contests/{id}/config/issues`
lists the same for a contest's own configs and those of its problems in it. The
problem's test case dialog and the contest's problem dialog show them, and a
judgement dispatched while the problem has any says so to staff
(`config_issues` on the submission's judgements, and a warning on the
submission page).

Your validator runs on every stored config of your plugin, and also where no
config is stored: for each namespace that declares `validate`, the host checks
its defaults at every target of the types your plugin provides (the contest
types, problem types and checker formats it registers) that stores none. A
contest-scope or contest-problem namespace is checked in every contest of your
contest type; a problem-scope namespace on every problem whose problem type,
checker format or default contest type you provide. So a problem your contest
type cannot judge shows up before anyone submits, even though nobody saved a
config for it.

Whether a problem has any is rechecked whenever the answer can change: on a
test data change; on every write or delete of one of its configs, at the
problem scope or in a contest, whoever makes it (an admin, your plugin's
`host.config.set*`, a checker source upload); when the problem is created, its
types change, it joins or leaves a contest, or a contest it is in changes its
contest type or is deleted; and, for every problem your checks reach, when your
plugin is enabled, disabled, reloaded or upgraded, when its plugin-scope config
is written or deleted, when a contest's own config is written or deleted, after
a bundle import, and when the server starts. A contest's config issues are
checked whenever they are read. While your plugin is disabled its configs are
not checked, so they count as having no issues until it is enabled again.

Only a server that loads your plugin can run its checks. Where your plugin is
enabled but not loaded on the server that checks (it failed to load there,
or its files have not reached it), the answer is unknown, and unknown reads as
having issues: the problem keeps its flag and is marked for a server that
loads your plugin, which decides on its next plugin reconcile pass. That
server's issues routes answer 503 `PLUGIN_UNAVAILABLE` with `Retry-After`
rather than an empty list.

A write to a problem's config holds the same lock a test data change does,
from its check until it commits, so a config is never stored against test data
that a concurrent change has just replaced.

## Move it with a bundle

A problem or contest bundle (`broccoli-server bundle export`) carries the
configs of what it exports: its problems' own and, for a contest, the
contest's and those of its problems in it. It never carries a plugin-scope
config: those are each server's own settings, and an import leaves the
target's as they are. An import replaces every problem and contest-problem
config the target had along with its problems, so no config of a replaced
problem stays to configure the imported problem that takes its id.

A bundle never carries a secret field (see _Keep a secret_). The export
withholds each one by the namespace's schema, read from the plugin's published
upload or the server's plugin directory, and lists where in the bundle, each
array item named by its key field. The configs of a plugin with neither, and a
config whose namespace its manifest no longer declares, stay out, and the
export names them. The rows carry each secret's ref, never its value. An import
keeps the secret the target stored, before the import replaced anything, at
the same item of the same config only where the target's ref is the bundle's:
the same stored secret. Re-importing a server's own export keeps every secret
it had, unless one was set again since. Every other withheld secret is left
unset, and the import's summary says how many. A database restored from
another's dump shares the other's refs only for the secrets neither set again
since, and a problem replaced at an id another problem held keeps none of
that problem's secrets.

An import runs no plugin, so it checks none of the configs it restores, and
the problems' `config_issues` flags do not travel: they describe the source's
plugins. An imported problem's issues are unknown until a server decides them,
so it lands flagged and marked, and a judgement claimed before then says it ran
with config issues. A running server decides the marked problems on its next
plugin reconcile pass, within a few seconds, without a restart.

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

`host.config.get_effective(namespace, problem_id, contest_id)` reads the
config a namespace has at one problem. Each scope that stores it sets the
fields it stores and leaves the rest to the next scope up, in the order
contest problem, contest, problem, the plugin-wide value, then the schema's
defaults: objects merge field by field, and any other value, an array
included, is replaced whole. The most specific scope whose row says whether
the config is on decides it, the plugin-wide row last. An override switched
off contributes nothing, and when the narrower scope that decides is switched
off the config is the plugin-wide one. `source` on the result names the most
specific scope the config takes fields from, and `fields` the scope each
top-level field comes from.

The rule is `resolve_cascade` in `broccoli-types`, and it is the one rule for
every reader: your plugin's reads, the admin forms, which show what each scope
inherits from `GET …/config/cascade` (`/problems/{id}`, `/contests/{id}` and
`/contests/{id}/problems/{problem_id}`), and the host's choice of whose
resource-scoped hooks run for a submission (a plugin runs when any of its
namespaces is on there).

## Mark what judging reads

Declare `judging_input = true` on every namespace your plugin reads while it
judges (sandbox limits, a checker's options, a task's subtasks):

```toml
[config.sandbox]
judging_input = true
scopes = ["plugin"]
```

A change to one of its stored configs then counts as a change to the
judging inputs of every problem the config reaches: a problem's or contest
problem's config that problem, a contest's config each problem of the
contest, and a plugin-scope config everything your plugin judges. A
judgement judged before
the change is applied only with `force` and shows as judged on earlier
inputs. A change to any other namespace (standings, display settings) leaves
judgements as they are. A `checker_source` namespace is a judging input
whether or not it says so.

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

If the checker compiles the source, name the checker formats that do with
`compiles`, and declare which language each primary file's extension compiles
in with `checker_languages`, both on the same namespace:

```toml
[config.checker_source]
scopes = ["problem"]
checker_source = true
compiles = ["testlib"]
checker_languages = { cpp = ["cpp", "cc", "cxx"], c = ["c"] }
```

A problem's checker language is the language of the first stored file whose
extension (the text after its last `.`) is listed, ASCII letters compared
case-insensitively. When several languages list that extension, the first by
name, bytewise, wins. The host derives it each
time it needs it, only for a problem whose checker format is listed in
`compiles` and whose fleet-wide provider is your plugin, so it always follows
the stored source. Work on the problem then needs that language's plugin as it
needs its own language's: while it is disabled, the work waits queued instead
of failing. Compile by the same rule, `broccoli_server_sdk::checker_language`,
so the language you compile in is the one the host waits for.

## Test it

`Host::mock()` reads the `plugin.toml` of the crate under test, so a native
test sees the same filled in config as the server:

```rust
let host = Host::mock();
host.config.seed("contest", "2", "contest", json!({"freeze_minutes": 15}));
let config: ContestConfig = host.config.get_contest(2, "contest")?.parse()?;
assert_eq!(config.penalty_minutes, 20);
```
